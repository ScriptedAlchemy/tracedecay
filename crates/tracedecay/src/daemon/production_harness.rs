//! In-process owner for the daemon's production project composition.
//!
//! Test and `test-transport` builds use this to drive the same composition the
//! daemon runs, against one isolated profile-and-projects root.

#[cfg(any(test, feature = "test-transport"))]
use std::future::Future;
#[cfg(any(test, feature = "test-transport"))]
use std::pin::Pin;

#[cfg(all(unix, any(test, feature = "test-transport")))]
use super::bootstrap::set_owner_only_permissions;
// The parent `daemon` module imports this under `cfg(test)` only, so
// `use super::*` cannot carry it into a `test-transport` build. Import it
// directly under the same gate the harness itself is compiled behind.
#[cfg(any(test, feature = "test-transport"))]
use super::project_composition::daemon_transcript_source_home;
#[cfg(any(test, feature = "test-transport"))]
use super::project_server_lifecycle::{detach_project_servers, shutdown_detached_project_servers};
#[cfg(any(test, feature = "test-transport"))]
use super::*;
#[cfg(any(test, feature = "test-transport"))]
use tracedecay_code_index_runtime::code_index_scheduler;
#[cfg(all(unix, feature = "test-transport"))]
use tracedecay_code_index_runtime::git_transactions;
#[cfg(any(test, feature = "test-transport"))]
use tracedecay_daemon_identity::profile_identity;

/// Captures the daemon's exact native Git transaction precondition for
/// transport-parity tests. This is not compiled into production builds.
#[cfg(all(unix, feature = "test-transport"))]
#[hotpath::measure(label = "daemon.harness.capture_git_snapshot")]
#[doc(hidden)]
pub fn capture_exact_git_snapshot_for_test(
    repository_root: &Path,
    project_id: tracedecay_domain::ProjectId,
    repository_id: tracedecay_domain::RepositoryId,
    worktree_id: tracedecay_domain::WorktreeId,
    captured_at: tracedecay_domain::UtcMicros,
) -> tracedecay_domain::errors::Result<tracedecay_domain::RepositoryStateSnapshotV1> {
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
    // Read by the `cfg(test)` capacity journey; under `test-transport` alone the
    // harness still must own the registry for its lifetime, so it reads as dead.
    #[cfg_attr(not(test), allow(dead_code))]
    http_application_registry: http_application::DaemonHttpApplicationRegistry,
    servers: HashMap<PathBuf, Arc<crate::mcp::McpServer>>,
    _database_scope: tracedecay_runtime_core::db::DaemonDatabaseScope,
    _lifecycle_lease: tracedecay_runtime_core::lifecycle_lease::LifecycleLease,
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

/// The isolated profile the composition owns inside one isolation root.
///
/// Resolvable before `open` so a caller can predict the composed layout.
#[cfg(any(test, feature = "test-transport"))]
fn composed_profile_root(isolation_root: &Path) -> PathBuf {
    isolation_root.join("profile")
}

/// Type-erased open future so a caller async body stores only a fat pointer.
///
/// A generic `async fn` open inlined the whole composition into every test
/// future; rustc then overflowed the layout-query depth budget.
#[cfg(any(test, feature = "test-transport"))]
type ProductionHarnessOpenFuture =
    Pin<Box<dyn Future<Output = Result<ProductionProjectCompositionHarnessV1>> + Send>>;

#[cfg(any(test, feature = "test-transport"))]
struct IsolatedProductionCompositionRoots {
    isolation_root: PathBuf,
    profile_root: PathBuf,
    project_roots: Vec<PathBuf>,
}

#[cfg(any(test, feature = "test-transport"))]
struct ProductionCompositionStoreHandles {
    store_administration: StoreAdministration,
    invocation: DaemonInvocationState,
    http_application_registry: http_application::DaemonHttpApplicationRegistry,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
}

#[cfg(any(test, feature = "test-transport"))]
fn isolate_production_composition_roots(
    isolation_root: PathBuf,
    project_roots: Vec<PathBuf>,
    live_profile_root: Option<PathBuf>,
) -> Result<IsolatedProductionCompositionRoots> {
    hotpath::measure_block!("daemon.harness.isolate", {
        std::fs::create_dir_all(&isolation_root).map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to create production-composition isolation root '{}': {error}",
                isolation_root.display()
            ),
        })?;
        let isolation_root =
            std::fs::canonicalize(&isolation_root).map_err(|error| TraceDecayError::Config {
                message: format!(
                    "failed to canonicalize production-composition isolation root '{}': {error}",
                    isolation_root.display()
                ),
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

        let profile_root = composed_profile_root(&isolation_root);
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
        Ok(IsolatedProductionCompositionRoots {
            isolation_root,
            profile_root,
            project_roots,
        })
    })
}

#[cfg(any(test, feature = "test-transport"))]
fn acquire_production_composition_identity(
    profile_root: &Path,
) -> Result<(
    profile_identity::LocalProfileIdentityAuthorityV1,
    tracedecay_runtime_core::lifecycle_lease::LifecycleLease,
    tracedecay_runtime_core::db::DaemonDatabaseScope,
)> {
    hotpath::measure_block!("daemon.harness.identity", {
        let profile_identity = profile_identity::load_or_create(profile_root)?;
        let lifecycle_lease = tracedecay_runtime_core::lifecycle_lease::acquire_shared_for_profile(
            profile_root,
            "in-process production composition",
        )?;
        let database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
            profile_root,
            1,
            "in-process-production-composition",
        )?;
        Ok((profile_identity, lifecycle_lease, database_scope))
    })
}

#[cfg(any(test, feature = "test-transport"))]
async fn install_production_composition_stores(
    profile_identity: profile_identity::LocalProfileIdentityAuthorityV1,
    long_lived_session_maintenance_for_test: bool,
) -> Result<ProductionCompositionStoreHandles> {
    let store_administration =
        StoreAdministration::default().with_profile_identity(profile_identity.clone());
    #[cfg(test)]
    if long_lived_session_maintenance_for_test {
        Box::pin(store_administration.install_long_lived_session_runtime_registry_for_test())
            .await?;
    }
    #[cfg(not(test))]
    let _ = long_lived_session_maintenance_for_test;
    let invocation = DaemonInvocationState::default();
    // The daemon bootstrap installs the Codex shared-JSONL preparation
    // authority right after creating the invocation state; without it every
    // transcript ingest refuses as background-resource unavailable. The
    // authority is a process singleton (a daemon restart is a new process),
    // so an in-process harness reopen must rejoin the memory authority the
    // first open installed rather than install a fresh one.
    static HARNESS_CODEX_PREPARATION_MEMORY: std::sync::OnceLock<
        Arc<tracedecay_runtime_core::resident_memory::ProcessResidentMemoryV1>,
    > = std::sync::OnceLock::new();
    let preparation_memory = Arc::clone(
        HARNESS_CODEX_PREPARATION_MEMORY
            .get_or_init(|| invocation.code_index_schedulers.process_resident_memory()),
    );
    store_administration
        .configure_codex_preparation_resources(preparation_memory)
        .map_err(|error| TraceDecayError::Config {
            message: format!("failed to configure Codex preparation resources: {error}"),
        })?;
    invocation.configure_github_read_only_credentials(&profile_identity);
    let http_application_registry = http_application::DaemonHttpApplicationRegistry::default();
    let project_open_gates = Arc::new(tokio::sync::Mutex::new(ProjectOpenGates::default()));
    Box::pin(install_production_composition_profile_workers(
        &store_administration,
        &invocation,
        &http_application_registry,
        &project_open_gates,
        &profile_identity,
    ))
    .await?;
    Ok(ProductionCompositionStoreHandles {
        store_administration,
        invocation,
        http_application_registry,
        project_open_gates,
    })
}

#[cfg(any(test, feature = "test-transport"))]
async fn install_production_composition_profile_workers(
    store_administration: &StoreAdministration,
    invocation: &DaemonInvocationState,
    http_application_registry: &http_application::DaemonHttpApplicationRegistry,
    project_open_gates: &Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    profile_identity: &profile_identity::LocalProfileIdentityAuthorityV1,
) -> Result<()> {
    hotpath::future!(
        async {
            let profile_sessions = store_administration
                .registered_profile_session_database()
                .await?;
            invocation
                .install_profile_worker_plan(profile_sessions, profile_identity.profile_id())
                .await?;
            store_administration.install_remote_recovery_project_lifecycle(
                invocation.clone(),
                Arc::clone(project_open_gates),
            )?;
            install_http_application_cold_resolver(
                http_application_registry,
                store_administration.clone(),
                invocation.clone(),
                Arc::clone(project_open_gates),
            )?;
            Ok::<(), TraceDecayError>(())
        },
        label = "daemon.harness.stores"
    )
    .await
}

#[cfg(any(test, feature = "test-transport"))]
async fn mount_production_composition_projects(
    stores: &ProductionCompositionStoreHandles,
    project_roots: Vec<PathBuf>,
    profile_root: &Path,
    scope_prefix: Option<String>,
    wait_for_code_index: bool,
) -> Result<(HashMap<PathBuf, Arc<crate::mcp::McpServer>>, bool)> {
    let client_identity = DaemonClientIdentity {
        profile_root: profile_root.to_path_buf(),
        global_db_path: profile_root.join("global.db"),
    };
    let mut servers = HashMap::new();
    let mut semantic_auto_download_enabled = false;
    for (index, project_root) in project_roots.into_iter().enumerate() {
        let (canonical_project_path, server, project_semantic) =
            Box::pin(mount_one_production_composition_project(
                stores,
                project_root,
                &client_identity,
                scope_prefix.as_deref(),
                index,
                wait_for_code_index,
            ))
            .await?;
        semantic_auto_download_enabled |= project_semantic;
        servers.insert(canonical_project_path, server);
    }
    Ok((servers, semantic_auto_download_enabled))
}

#[cfg(any(test, feature = "test-transport"))]
async fn mount_one_production_composition_project(
    stores: &ProductionCompositionStoreHandles,
    project_root: PathBuf,
    client_identity: &DaemonClientIdentity,
    scope_prefix: Option<&str>,
    index: usize,
    wait_for_code_index: bool,
) -> Result<(PathBuf, Arc<crate::mcp::McpServer>, bool)> {
    let handshake = DaemonHandshake {
        client_version: binary_version()?.to_owned(),
        client_instance_id: format!("production-composition-harness-{index}"),
        client_identity: client_identity.clone(),
        scope_prefix: scope_prefix.map(str::to_owned),
        project_path: Some(project_root),
        timings: false,
        allow_init: true,
        allow_initialize_root_routing: false,
        tool_list_changed_capable: false,
        catalog_version: String::new(),
        moved_store_adoption: crate::tracedecay::MovedStoreAdoption::Never,
    };
    let (canonical_project_path, _) = project_route_for_handshake(&handshake)?;
    let composition = stores
        .store_administration
        .with_writer(|| {
            let store_administration = &stores.store_administration;
            let project_open_gates = &stores.project_open_gates;
            let invocation = &stores.invocation;
            let http_application_registry = &stores.http_application_registry;
            let canonical_project_path = &canonical_project_path;
            let handshake = &handshake;
            Box::pin(async move {
                let cancellation = CancellationToken::new();
                production_project_server(
                    store_administration,
                    project_open_gates,
                    invocation,
                    http_application_registry,
                    canonical_project_path,
                    handshake,
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
        })
        .await?;
    if wait_for_code_index {
        let code_search_scope = {
            let graph = composition.server.cg().await;
            let target = graph.configuration_runtime().configuration_target();
            tracedecay_code_index_runtime::resolved_scope_for_project(
                graph.project_root(),
                &target.project_id,
            )
            .map_err(|error| TraceDecayError::Config {
                message: format!("production-composition code-index scope is invalid: {error:?}"),
            })?
        };
        Box::pin(wait_for_production_composition_code_index(
            &stores.invocation,
            &composition.canonical_project_path,
            &code_search_scope,
        ))
        .await?;
    }
    let semantic_auto_download_enabled =
        composition
            .semantic_auto_download_enabled
            .ok_or_else(|| TraceDecayError::Config {
                message: "production-composition harness reused an unobserved semantic runtime"
                    .to_owned(),
            })?;
    Ok((
        composition.canonical_project_path,
        composition.server,
        semantic_auto_download_enabled,
    ))
}

#[cfg(any(test, feature = "test-transport"))]
impl ProductionProjectCompositionHarnessV1 {
    /// Where the composed daemon reads host transcripts from, resolvable
    /// before `open`.
    ///
    /// The composition pins its transcript source home to its own isolated
    /// layout rather than reading the ambient process `HOME`, so a journey
    /// that seeds a real transcript must write it here — a transcript written
    /// under `$HOME` is invisible to the composition and the session lane
    /// stays empty forever.
    pub fn transcript_source_home(isolation_root: impl AsRef<Path>) -> Option<PathBuf> {
        daemon_transcript_source_home(&composed_profile_root(isolation_root.as_ref()))
    }

    pub fn open(
        isolation_root: impl AsRef<Path>,
        project_roots: impl IntoIterator<Item = PathBuf>,
    ) -> ProductionHarnessOpenFuture {
        let live_profile_root = crate::config::user_data_dir().filter(|path| path.exists());
        Self::open_with_live_profile_root(
            isolation_root.as_ref().to_path_buf(),
            project_roots.into_iter().collect(),
            live_profile_root,
            None,
            false,
            true,
        )
    }

    /// Opens the production composition without making unrelated code-index
    /// readiness a precondition for session-only lifecycle journeys.
    #[doc(hidden)]
    pub fn open_for_session_retrieval(
        isolation_root: impl AsRef<Path>,
        project_roots: impl IntoIterator<Item = PathBuf>,
    ) -> ProductionHarnessOpenFuture {
        let live_profile_root = crate::config::user_data_dir().filter(|path| path.exists());
        Self::open_with_live_profile_root(
            isolation_root.as_ref().to_path_buf(),
            project_roots.into_iter().collect(),
            live_profile_root,
            None,
            false,
            false,
        )
    }

    pub fn open_with_scope_prefix(
        isolation_root: impl AsRef<Path>,
        project_roots: impl IntoIterator<Item = PathBuf>,
        scope_prefix: impl Into<String>,
    ) -> ProductionHarnessOpenFuture {
        let live_profile_root = crate::config::user_data_dir().filter(|path| path.exists());
        Self::open_with_live_profile_root(
            isolation_root.as_ref().to_path_buf(),
            project_roots.into_iter().collect(),
            live_profile_root,
            Some(scope_prefix.into()),
            false,
            true,
        )
    }

    fn open_with_live_profile_root(
        isolation_root: PathBuf,
        project_roots: Vec<PathBuf>,
        live_profile_root: Option<PathBuf>,
        scope_prefix: Option<String>,
        long_lived_session_maintenance_for_test: bool,
        wait_for_code_index: bool,
    ) -> ProductionHarnessOpenFuture {
        // Embedded test compositions never pass through the binary's
        // product-runtime registration, so the canonical fixture is this
        // composition's provider; without it daemon bootstrap and version
        // reporting answer the typed missing-provider state.
        crate::product_runtime::register_fixture_product_runtime();
        Box::pin(async move {
            let isolated = isolate_production_composition_roots(
                isolation_root,
                project_roots,
                live_profile_root,
            )?;
            let (profile_identity, lifecycle_lease, database_scope) =
                acquire_production_composition_identity(&isolated.profile_root)?;
            let stores = Box::pin(install_production_composition_stores(
                profile_identity,
                long_lived_session_maintenance_for_test,
            ))
            .await?;
            let (servers, semantic_auto_download_enabled) =
                Box::pin(mount_production_composition_projects(
                    &stores,
                    isolated.project_roots,
                    &isolated.profile_root,
                    scope_prefix,
                    wait_for_code_index,
                ))
                .await?;
            Ok(Self {
                isolation_root: isolated.isolation_root,
                profile_root: isolated.profile_root,
                semantic_auto_download_enabled,
                resources: Some(ProductionProjectHarnessResourcesV1 {
                    store_administration: stores.store_administration,
                    invocation: stores.invocation,
                    _project_open_gates: stores.project_open_gates,
                    http_application_registry: stores.http_application_registry,
                    servers,
                    _database_scope: database_scope,
                    _lifecycle_lease: lifecycle_lease,
                }),
            })
        })
    }

    #[cfg(test)]
    pub(super) fn open_with_live_profile_root_for_test(
        isolation_root: impl AsRef<Path>,
        project_roots: impl IntoIterator<Item = PathBuf>,
        live_profile_root: PathBuf,
    ) -> ProductionHarnessOpenFuture {
        Self::open_with_live_profile_root(
            isolation_root.as_ref().to_path_buf(),
            project_roots.into_iter().collect(),
            Some(live_profile_root),
            None,
            false,
            true,
        )
    }

    #[cfg(test)]
    pub(super) fn open_with_session_maintenance_for_test(
        isolation_root: impl AsRef<Path>,
        project_roots: impl IntoIterator<Item = PathBuf>,
    ) -> ProductionHarnessOpenFuture {
        Self::open_with_live_profile_root(
            isolation_root.as_ref().to_path_buf(),
            project_roots.into_iter().collect(),
            None,
            None,
            true,
            true,
        )
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

    #[hotpath::measure(label = "daemon.harness.read_profile_analytics", future = true)]
    pub async fn read_profile_analytics_events(
        &self,
        query: &tracedecay_global_db::AnalyticsEventQuery,
    ) -> Result<Vec<tracedecay_global_db::AnalyticsEventRecord>> {
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

    /// Seeds exact retained analytics rows through the mounted profile
    /// database authority for production-composition transport tests.
    #[hotpath::measure(label = "daemon.harness.append_profile_analytics", future = true)]
    pub async fn append_profile_analytics_events_for_test(
        &self,
        events: &[tracedecay_global_db::AnalyticsEventInsert],
    ) -> Result<Vec<i64>> {
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
            .append_analytics_events(events)
            .await
            .map_err(|message| TraceDecayError::Database {
                message,
                operation: "seed retained production profile analytics".to_owned(),
            })
    }

    /// Sums the retained profile's settled savings-ledger rows, optionally
    /// scoped to one project path — the production accounting authority the
    /// MCP analytics journeys assert against.
    #[hotpath::measure(label = "daemon.harness.sum_profile_savings", future = true)]
    pub async fn sum_profile_savings(
        &self,
        project: Option<&str>,
        since: i64,
    ) -> Result<tracedecay_global_db::SavingsTotal> {
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
            .sum_savings(project, since)
            .await
            .map_err(|message| TraceDecayError::Database {
                message,
                operation: "sum retained profile savings".to_owned(),
            })
    }

    /// Reads one project's lifetime saved-token counter from the retained
    /// profile authority.
    #[hotpath::measure(label = "daemon.harness.project_lifetime_saved_tokens", future = true)]
    pub async fn project_lifetime_saved_tokens(&self, project_root: &Path) -> Result<u64> {
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
            .try_get_project_tokens(project_root)
            .await
            .map_err(|message| TraceDecayError::Database {
                message,
                operation: "read project lifetime saved tokens".to_owned(),
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

    #[hotpath::skip]
    pub async fn project_data_root(&self, project_root: impl AsRef<Path>) -> Result<PathBuf> {
        Ok(self
            .server(project_root)?
            .cg()
            .await
            .store_layout()
            .data_root
            .clone())
    }

    /// The mounted project's registered identity, which is the only accepted
    /// cross-project selector: tools reject a top-level `project_path`, so a
    /// caller routing to a second mounted project must pass
    /// `project_selector.project_id`.
    #[hotpath::skip]
    pub async fn project_id(&self, project_root: impl AsRef<Path>) -> Result<String> {
        let project_root = project_root.as_ref().to_path_buf();
        self.server(&project_root)?
            .cg()
            .await
            .store_layout()
            .identity
            .project_id
            .clone()
            .ok_or_else(|| TraceDecayError::Config {
                message: format!(
                    "production-composition project '{}' has no registered project identity",
                    project_root.display()
                ),
            })
    }

    #[hotpath::measure(label = "daemon.harness.track_worktree_branch", future = true)]
    pub async fn track_worktree_branch(
        &self,
        project_root: impl AsRef<Path>,
        worktree_root: impl AsRef<Path>,
        branch: &str,
    ) -> Result<tracedecay_runtime_core::branch::BranchAddOutcome> {
        let canonical_project_root =
            std::fs::canonicalize(project_root.as_ref()).map_err(|error| {
                TraceDecayError::Config {
                    message: format!(
                        "failed to canonicalize production-composition project '{}': {error}",
                        project_root.as_ref().display()
                    ),
                }
            })?;
        let graph = self.server(&canonical_project_root)?.cg().await;
        let resources = self
            .resources
            .as_ref()
            .ok_or_else(|| TraceDecayError::Config {
                message: "production-composition harness is shut down".to_owned(),
            })?;
        super::branch_add::track_exact_worktree_branch(
            &graph,
            &resources.invocation.code_index_schedulers,
            &canonical_project_root,
            worktree_root.as_ref(),
            branch,
        )
        .await
    }

    #[hotpath::measure(label = "daemon.harness.sync_worktree_branch", future = true)]
    pub async fn sync_tracked_worktree_branch(
        &self,
        project_root: impl AsRef<Path>,
        worktree_root: impl AsRef<Path>,
        branch: &str,
        query: &str,
    ) -> Result<(Option<String>, Option<String>, bool, bool)> {
        self.track_worktree_branch(project_root.as_ref(), worktree_root.as_ref(), branch)
            .await?;
        let canonical_project_root =
            std::fs::canonicalize(project_root.as_ref()).map_err(|error| {
                TraceDecayError::Config {
                    message: format!(
                        "failed to canonicalize production-composition project '{}': {error}",
                        project_root.as_ref().display()
                    ),
                }
            })?;
        let canonical_worktree_root =
            std::fs::canonicalize(worktree_root.as_ref()).map_err(|error| {
                TraceDecayError::Config {
                    message: format!(
                        "failed to canonicalize branch worktree '{}': {error}",
                        worktree_root.as_ref().display()
                    ),
                }
            })?;
        let graph = self.server(&canonical_project_root)?.cg().await;
        let resources = self
            .resources
            .as_ref()
            .ok_or_else(|| TraceDecayError::Config {
                message: "production-composition harness is shut down".to_owned(),
            })?;
        let source = super::branch_add::capture_exact_branch_source(
            &graph,
            &resources.invocation.code_index_schedulers,
            &canonical_project_root,
            &canonical_worktree_root,
            branch,
        )
        .await?;
        let generation = super::branch_add::await_exact_branch_generation(
            &resources.invocation.code_index_schedulers,
            &canonical_worktree_root,
            &source,
        )
        .await?;
        let contains_query = generation.symbols().symbols.iter().any(|symbol| {
            symbol.simple_name.contains(query) || symbol.qualified_name.contains(query)
        });
        Ok((
            tracedecay_runtime_core::branch::current_branch(&canonical_worktree_root),
            source
                .reference
                .strip_prefix("refs/heads/")
                .map(str::to_owned),
            false,
            contains_query,
        ))
    }

    #[hotpath::measure(label = "daemon.harness.call_tool", future = true)]
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

    #[hotpath::skip]
    pub async fn shutdown(mut self) {
        if let Some(resources) = self.resources.take() {
            hotpath::future!(
                shutdown_production_project_harness(resources),
                label = "daemon.harness.shutdown"
            )
            .await;
        }
    }
}

#[cfg(any(test, feature = "test-transport"))]
#[hotpath::measure(label = "daemon.harness.wait_code_index", future = true)]
async fn wait_for_production_composition_code_index(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    scope: &tracedecay_application::ResolvedScope,
) -> Result<Option<code_index_scheduler::LatestCompleteCodeIndexV1>> {
    timeout(Duration::from_secs(20), async {
        loop {
            // Scope-aware readiness is the authenticated demand boundary that
            // starts the registered route-local activation owner. The root-only
            // probe cannot mount an idle on-demand scheduler.
            if let Some(latest) = invocation
                .code_index_schedulers
                .latest_complete_ready_for_scope(scope)
                .await
            {
                return Some(latest);
            }
            // A project whose verified source publishes no generation at all
            // (every file unsupported or unextractable) is a typed state, not
            // a warming one: waiting for its publication would always exhaust
            // the timeout. The composition still mounts; graph-backed reads
            // then report their typed generation-unavailable refusals.
            if invocation
                .code_index_schedulers
                .reconciled_without_generation_for_scope(scope)
                .await
            {
                return None;
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
            runtime.spawn(hotpath::future!(
                shutdown_production_project_harness(resources),
                label = "daemon.harness.shutdown"
            ));
        }
    }
}

#[cfg(any(test, feature = "test-transport"))]
async fn shutdown_production_project_harness(mut resources: ProductionProjectHarnessResourcesV1) {
    resources
        .store_administration
        .join_project_server_retirements()
        .await;
    let servers = hotpath::future!(
        detach_project_servers(&resources.store_administration),
        label = "daemon.harness.detach"
    )
    .await;
    resources.servers.clear();
    for server in &servers {
        server.ledger_writes_settled().await;
        server.shutdown_background_tasks().await;
    }
    hotpath::future!(
        async {
            resources
                .store_administration
                .session_temporal_refresh_schedulers()
                .shutdown()
                .await;
            resources.store_administration.shutdown_session_sync().await;
            resources
                .store_administration
                .shutdown_host_admission_replay()
                .await;
        },
        label = "daemon.harness.shutdown_sessions"
    )
    .await;
    hotpath::future!(
        resources.invocation.shutdown(),
        label = "daemon.harness.shutdown_invocation"
    )
    .await;
    hotpath::future!(
        shutdown_detached_project_servers(
            tokio::time::Instant::now() + tracedecay_runtime_core::DAEMON_SHUTDOWN_DEADLINE,
            servers,
        ),
        label = "daemon.harness.shutdown_detached"
    )
    .await;
    match resources
        .store_administration
        .prepare_memory_graph_reconciliation_shutdown()
        .await
    {
        Ok(owner) => {
            owner.cancel();
            hotpath::future!(
                async {
                    if let Err(error) = owner.shutdown().await {
                        tracing::warn!(
                            event = "production_harness_graph_shutdown_failed",
                            error = %error,
                            "production-composition graph reconciliation tasks did not stop cleanly"
                        );
                    }
                    if let Err(error) = resources
                        .store_administration
                        .close_retained_graph_runtimes_for_shutdown()
                        .await
                    {
                        tracing::warn!(
                            event = "production_harness_graph_shutdown_failed",
                            error = %error,
                            "production-composition graph runtimes did not close cleanly"
                        );
                    }
                },
                label = "daemon.harness.shutdown_graph"
            )
            .await;
        }
        Err(error) => tracing::warn!(
            event = "production_harness_graph_shutdown_failed",
            error = %error,
            "production-composition graph shutdown owner was unavailable"
        ),
    }
    drop(resources);
}

#[cfg(test)]
mod code_index_activation_test {
    use std::process::Command;

    use tempfile::TempDir;

    use super::*;

    #[tokio::test]
    #[hotpath::skip]
    async fn fresh_profile_first_reconcile_makes_query_authority_ready_within_existing_bound() {
        let isolation = TempDir::new().expect("production harness isolation");
        let project = isolation.path().join("project");
        std::fs::create_dir_all(&project).expect("project root");
        std::fs::write(project.join("lib.rs"), "pub fn indexed_symbol() {}\n")
            .expect("project source");
        for arguments in [
            vec!["init", "-q"],
            vec!["add", "."],
            vec![
                "-c",
                "user.name=TraceDecay Test",
                "-c",
                "user.email=tracedecay@example.invalid",
                "commit",
                "-qm",
                "seed project",
            ],
        ] {
            let status = Command::new(
                tracedecay_runtime_core::git::try_git_program()
                    .expect("absolute git executable should resolve"),
            )
            .args(&arguments)
            .current_dir(&project)
            .status()
            .expect("git fixture command");
            assert!(status.success(), "git {arguments:?}");
        }

        let harness =
            ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
                .await
                .expect("production harness activates its code index");
        let worker_status = tracedecay_code_index::parallelism::installed_worker_status()
            .expect("production harness worker plan");
        assert_eq!(
            worker_status.configured,
            tracedecay_domain::configuration::CodeIndexWorkerSelectionV1::Automatic {},
            "a fresh harness must install the profile-scoped default selection"
        );
        let query = timeout(Duration::from_secs(20), async {
            loop {
                let response = harness
                    .call_tool(
                        &project,
                        "tracedecay_search",
                        json!({
                            "query": "indexed_symbol",
                            "limit": 1,
                            "format": "json",
                        }),
                    )
                    .await
                    .expect("fresh-profile production search response");
                let payload = super::journey_test_support::tool_payload(&response);
                let served_symbol = payload["results"].as_array().is_some_and(|results| {
                    results
                        .iter()
                        .any(|result| result["display"]["name"] == json!("indexed_symbol"))
                });
                if served_symbol {
                    break payload;
                }
                if payload["status"] == json!("unavailable") {
                    let retryable = matches!(
                        payload["reason"].as_str(),
                        Some(
                            "authority_unavailable"
                                | "generation_unavailable"
                                | "generation_unverified"
                                | "search_capacity_unavailable"
                        )
                    );
                    assert!(
                        retryable,
                        "fresh-profile production search failed terminally: {payload}"
                    );
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    continue;
                }
                panic!(
                    "fresh-profile production search completed without indexed_symbol: {payload}"
                );
            }
        })
        .await
        .expect("fresh-profile query authority becomes ready within the existing bound");
        assert!(
            query["results"].as_array().is_some_and(|results| {
                results
                    .iter()
                    .any(|result| result["display"]["name"] == json!("indexed_symbol"))
            }),
            "fresh-profile production search did not serve the indexed symbol: {query}"
        );
        harness.shutdown().await;
    }
}

#[cfg(test)]
mod project_server_capacity_journey_test;

#[cfg(test)]
mod journey_test_support;

#[cfg(test)]
mod delivery_read_gate_journey_test;

#[cfg(test)]
mod generation_retention_test;

#[cfg(test)]
mod configuration_idempotency_journey_test;

#[cfg(test)]
mod semantic_activation_journey_test;

#[cfg(test)]
mod semantic_availability_journey_test;

#[cfg(test)]
mod semantic_index_fixture_check_test;
