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
// The parent `daemon` module imports this under `cfg(test)` only, so
// `use super::*` cannot carry it into a `test-transport` build. Import it
// directly under the same gate the harness itself is compiled behind.
#[cfg(any(test, feature = "test-transport"))]
use super::project_composition::daemon_transcript_source_home;
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

/// The isolated profile the composition owns inside one isolation root.
///
/// Resolvable before `open` so a caller can predict the composed layout.
#[cfg(any(test, feature = "test-transport"))]
fn composed_profile_root(isolation_root: &Path) -> PathBuf {
    isolation_root.join("profile")
}

#[cfg(any(test, feature = "test-transport"))]
struct ExactBranchGenerationV1 {
    generation: Arc<crate::code_index::production::CodeIndexPublishedGenerationV1>,
    installed_generation: Option<tracedecay_domain::CodeGenerationId>,
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

    pub async fn open(
        isolation_root: impl AsRef<Path>,
        project_roots: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Self> {
        let live_profile_root = crate::config::user_data_dir().filter(|path| path.exists());
        Self::open_with_live_profile_root(isolation_root, project_roots, live_profile_root, None)
            .await
    }

    pub async fn open_with_scope_prefix(
        isolation_root: impl AsRef<Path>,
        project_roots: impl IntoIterator<Item = PathBuf>,
        scope_prefix: impl Into<String>,
    ) -> Result<Self> {
        let live_profile_root = crate::config::user_data_dir().filter(|path| path.exists());
        Self::open_with_live_profile_root(
            isolation_root,
            project_roots,
            live_profile_root,
            Some(scope_prefix.into()),
        )
        .await
    }

    async fn open_with_live_profile_root(
        isolation_root: impl AsRef<Path>,
        project_roots: impl IntoIterator<Item = PathBuf>,
        live_profile_root: Option<PathBuf>,
        scope_prefix: Option<String>,
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
        let project_open_gates = Arc::new(tokio::sync::Mutex::new(ProjectOpenGates::default()));
        store_administration.install_remote_recovery_project_lifecycle(
            invocation.clone(),
            Arc::clone(&project_open_gates),
        )?;
        install_http_application_cold_resolver(
            &http_application_registry,
            store_administration.clone(),
            invocation.clone(),
            Arc::clone(&project_open_gates),
        )?;
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
                scope_prefix: scope_prefix.clone(),
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
            let code_search_scope = {
                let graph = composition.server.cg().await;
                let target = graph.configuration_runtime().configuration_target();
                project_open_owners::resolved_scope_for_project(
                    graph.project_root(),
                    &target.project_id,
                )
                .map_err(|error| TraceDecayError::Config {
                    message: format!(
                        "production-composition code-index scope is invalid: {error:?}"
                    ),
                })?
            };
            wait_for_production_composition_code_index(
                &invocation,
                &composition.canonical_project_path,
                &code_search_scope,
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
        Self::open_with_live_profile_root(
            isolation_root,
            project_roots,
            Some(live_profile_root),
            None,
        )
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

    /// Seeds exact retained analytics rows through the mounted profile
    /// database authority for production-composition transport tests.
    pub async fn append_profile_analytics_events_for_test(
        &self,
        events: &[crate::global_db::AnalyticsEventInsert],
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
    pub async fn sum_profile_savings(
        &self,
        project: Option<&str>,
        since: i64,
    ) -> Result<crate::global_db::SavingsTotal> {
        let resources = self
            .resources
            .as_ref()
            .ok_or_else(|| TraceDecayError::Config {
                message: "production-composition harness is shut down".to_owned(),
            })?;
        Ok(resources
            .store_administration
            .registered_profile_database()
            .await?
            .sum_savings(project, since)
            .await)
    }

    /// Reads one project's lifetime saved-token counter from the retained
    /// profile authority.
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
        let (data_root, project_id) = {
            let graph = self.server(&canonical_project_root)?.cg().await;
            let layout = graph.store_layout();
            let project_id =
                layout
                    .identity
                    .project_id
                    .clone()
                    .ok_or_else(|| TraceDecayError::Config {
                        message:
                            "branch graph publication requires an authoritative project identity"
                                .to_owned(),
                    })?;
            (layout.data_root.clone(), project_id)
        };
        let source = self
            .capture_exact_branch_source(
                &canonical_project_root,
                &canonical_worktree_root,
                &project_id,
                branch,
            )
            .await?;
        let prepared = crate::branch::prepare_branch_tracking_in_layout(
            &canonical_worktree_root,
            branch,
            &data_root,
        )
        .await?;
        let prepared = match prepared {
            crate::branch::BranchTrackingPreparation::Added(prepared) => Some(prepared),
            crate::branch::BranchTrackingPreparation::AlreadyTracked => None,
            crate::branch::BranchTrackingPreparation::Deferred => {
                return Ok(crate::branch::BranchAddOutcome::Deferred);
            }
        };
        let expected_source = crate::branch_meta::load_branch_meta(&data_root).and_then(|meta| {
            meta.branches
                .get(branch)
                .and_then(|entry| entry.graph_source.clone())
        });
        if expected_source
            .as_ref()
            .is_some_and(|existing| existing.matches_draft(&source))
        {
            return Ok(crate::branch::BranchAddOutcome::AlreadyTracked);
        }

        let exact_generation = match self
            .await_exact_branch_generation(&canonical_worktree_root, &source)
            .await
        {
            Ok(generation) => generation,
            Err(error) => {
                self.rollback_failed_branch_tracking(
                    &data_root,
                    &canonical_worktree_root,
                    prepared.as_ref(),
                    None,
                    &error,
                )
                .await?;
                return Err(error);
            }
        };
        let publication = crate::branch_meta::publish_graph_source(
            &data_root,
            branch,
            expected_source.as_ref(),
            source.clone(),
        )
        .map_err(|error| TraceDecayError::Config {
            message: format!("branch graph source publication failed: {error}"),
        });
        match publication {
            Ok(crate::branch_meta::BranchGraphSourcePublishOutcomeV1::Published(_)) => {
                Ok(crate::branch::BranchAddOutcome::Added)
            }
            Ok(crate::branch_meta::BranchGraphSourcePublishOutcomeV1::AlreadyPublished(_)) => {
                Ok(crate::branch::BranchAddOutcome::AlreadyTracked)
            }
            Ok(crate::branch_meta::BranchGraphSourcePublishOutcomeV1::CompareAndSwapMiss {
                observed: Some(observed),
            }) if observed.matches_draft(&source) => {
                Ok(crate::branch::BranchAddOutcome::AlreadyTracked)
            }
            Ok(outcome) => {
                let error = TraceDecayError::Config {
                    message: format!(
                        "branch graph source publication did not commit exact provenance for '{branch}': {outcome:?}"
                    ),
                };
                self.rollback_failed_branch_tracking(
                    &data_root,
                    &canonical_worktree_root,
                    prepared.as_ref(),
                    exact_generation.installed_generation.as_ref(),
                    &error,
                )
                .await?;
                Err(error)
            }
            Err(error) => {
                self.rollback_failed_branch_tracking(
                    &data_root,
                    &canonical_worktree_root,
                    prepared.as_ref(),
                    exact_generation.installed_generation.as_ref(),
                    &error,
                )
                .await?;
                Err(error)
            }
        }
    }

    pub async fn sync_tracked_worktree_branch(
        &self,
        project_root: impl AsRef<Path>,
        worktree_root: impl AsRef<Path>,
        branch: &str,
        query: &str,
    ) -> Result<(Option<String>, Option<String>, bool, bool)> {
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
        let project_id = {
            let graph = self.server(&canonical_project_root)?.cg().await;
            graph
                .store_layout()
                .identity
                .project_id
                .clone()
                .ok_or_else(|| TraceDecayError::Config {
                    message: "branch graph publication requires an authoritative project identity"
                        .to_owned(),
                })?
        };
        let source = self
            .capture_exact_branch_source(
                &canonical_project_root,
                &canonical_worktree_root,
                &project_id,
                branch,
            )
            .await?;
        let generation = self
            .await_exact_branch_generation(&canonical_worktree_root, &source)
            .await?
            .generation;
        let contains_query = generation.symbols().symbols.iter().any(|symbol| {
            symbol.simple_name.contains(query) || symbol.qualified_name.contains(query)
        });
        Ok((
            crate::branch::current_branch(&canonical_worktree_root),
            source
                .reference
                .strip_prefix("refs/heads/")
                .map(str::to_owned),
            false,
            contains_query,
        ))
    }

    async fn capture_exact_branch_source(
        &self,
        canonical_project_root: &Path,
        canonical_worktree_root: &Path,
        project_id: &str,
        branch: &str,
    ) -> Result<crate::branch_meta::BranchGraphSourceDraftV1> {
        let resources = self
            .resources
            .as_ref()
            .ok_or_else(|| TraceDecayError::Config {
                message: "production-composition harness is shut down".to_owned(),
            })?;
        let schedulers = &resources.invocation.code_index_schedulers;
        let scope = schedulers
            .serving_code_scope(canonical_worktree_root)
            .await
            .ok_or_else(|| {
                TraceDecayError::project_route(
                    "code_index_scheduler_unavailable",
                    true,
                    format!(
                        "code-index scheduler authority is unavailable for branch worktree '{}' in project '{}'",
                        canonical_worktree_root.display(),
                        canonical_project_root.display()
                    ),
                )
            })?;
        if scope
            .shutting_down
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(TraceDecayError::project_route(
                "code_index_scheduler_unavailable",
                true,
                format!(
                    "code-index scheduler is shutting down for branch worktree '{}'",
                    canonical_worktree_root.display()
                ),
            ));
        }
        let project_identity = tracedecay_domain::ProjectId::new(project_id.to_owned()).map_err(
            |error| TraceDecayError::Config {
                message: format!(
                    "branch graph publication has an invalid project identity '{project_id}': {error}"
                ),
            },
        )?;
        let snapshot = git_transactions::capture_exact_snapshot(
            canonical_worktree_root,
            project_identity.clone(),
            scope.repository_id.clone(),
            scope.worktree_id.clone(),
            tracedecay_application::now_micros(),
        )
        .map_err(|error| {
            TraceDecayError::project_route(
                "git_snapshot_unavailable",
                true,
                format!(
                    "failed to capture exact Git snapshot for branch worktree '{}': {error}",
                    canonical_worktree_root.display()
                ),
            )
        })?;
        if snapshot.project_id != project_identity
            || snapshot.repository_id != scope.repository_id
            || snapshot.worktree_id.as_ref() != Some(&scope.worktree_id)
        {
            return Err(TraceDecayError::project_route(
                "code_index_scheduler_identity_mismatch",
                true,
                format!(
                    "exact Git snapshot does not match the mounted scheduler route for '{}'",
                    canonical_worktree_root.display()
                ),
            ));
        }
        let (snapshot_branch, source_oid) = match snapshot.head {
            tracedecay_domain::GitHeadStateV1::Attached { branch, commit } => {
                (branch, commit.as_str().to_owned())
            }
            tracedecay_domain::GitHeadStateV1::Detached { .. }
            | tracedecay_domain::GitHeadStateV1::Unborn { .. } => {
                return Err(TraceDecayError::project_route(
                    "git_snapshot_unavailable",
                    true,
                    format!(
                        "branch graph publication requires an attached committed head for '{}'",
                        canonical_worktree_root.display()
                    ),
                ));
            }
        };
        if snapshot_branch != branch {
            return Err(TraceDecayError::project_route(
                "code_index_scheduler_identity_mismatch",
                true,
                format!(
                    "exact Git snapshot is attached to branch '{snapshot_branch}', not requested branch '{branch}'"
                ),
            ));
        }
        Ok(crate::branch_meta::BranchGraphSourceDraftV1 {
            project_id: project_id.to_owned(),
            repository_id: scope.repository_id.as_str().to_owned(),
            worktree_id: scope.worktree_id.as_str().to_owned(),
            worktree_root: canonical_worktree_root.to_string_lossy().into_owned(),
            reference: format!("refs/heads/{branch}"),
            source_oid,
        })
    }

    async fn await_exact_branch_generation(
        &self,
        canonical_worktree_root: &Path,
        source: &crate::branch_meta::BranchGraphSourceDraftV1,
    ) -> Result<ExactBranchGenerationV1> {
        let resources = self
            .resources
            .as_ref()
            .ok_or_else(|| TraceDecayError::Config {
                message: "production-composition harness is shut down".to_owned(),
            })?;
        let schedulers = &resources.invocation.code_index_schedulers;
        if let Some(generation) = schedulers
            .serving_code_scope(canonical_worktree_root)
            .await
            .and_then(|scope| scope.serving_generation)
            .filter(|generation| Self::generation_matches_branch_source(generation, source))
        {
            return Ok(ExactBranchGenerationV1 {
                generation,
                installed_generation: None,
            });
        }
        let mut publications = schedulers.subscribe_generation_publications();
        if !schedulers
            .notify_hook_overflow(canonical_worktree_root)
            .await
        {
            return Err(TraceDecayError::project_route(
                "code_index_scheduler_unavailable",
                true,
                format!(
                    "code-index scheduler rejected refresh for branch worktree '{}'",
                    canonical_worktree_root.display()
                ),
            ));
        }
        let generation = timeout(Duration::from_secs(20), async {
            loop {
                if let Some(generation) = schedulers
                    .serving_code_scope(canonical_worktree_root)
                    .await
                    .and_then(|scope| scope.serving_generation)
                    .filter(|generation| Self::generation_matches_branch_source(generation, source))
                {
                    return Ok(generation);
                }
                match publications.recv().await {
                    Ok(event) if event.project_root == canonical_worktree_root => {}
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        return Err(TraceDecayError::project_route(
                            "code_index_activation_unavailable",
                            true,
                            format!(
                                "code-index publication stream closed for branch worktree '{}'",
                                canonical_worktree_root.display()
                            ),
                        ));
                    }
                }
            }
        })
        .await
        .map_err(|_| {
            TraceDecayError::project_route(
                "code_index_activation_unavailable",
                true,
                format!(
                    "code-index scheduler did not publish exact branch source '{}' at '{}' for '{}'",
                    source.reference,
                    source.source_oid,
                    canonical_worktree_root.display()
                ),
            )
        })??;
        Ok(ExactBranchGenerationV1 {
            installed_generation: Some(generation.manifest().generation_id.clone()),
            generation,
        })
    }

    fn generation_matches_branch_source(
        generation: &crate::code_index::production::CodeIndexPublishedGenerationV1,
        source: &crate::branch_meta::BranchGraphSourceDraftV1,
    ) -> bool {
        let snapshot = generation.snapshot();
        snapshot.repository.as_str() == source.repository_id
            && snapshot
                .worktree
                .as_ref()
                .map(tracedecay_domain::WorktreeId::as_str)
                == Some(source.worktree_id.as_str())
            && snapshot
                .reference
                .as_ref()
                .map(tracedecay_domain::RefId::as_str)
                == Some(source.reference.as_str())
            && snapshot
                .source_revision
                .as_ref()
                .map(tracedecay_domain::GitOidV1::as_str)
                == Some(source.source_oid.as_str())
    }

    async fn rollback_failed_branch_tracking(
        &self,
        data_root: &Path,
        canonical_worktree_root: &Path,
        prepared: Option<&crate::branch::PreparedBranchTracking>,
        installed_generation: Option<&tracedecay_domain::CodeGenerationId>,
        cause: &TraceDecayError,
    ) -> Result<()> {
        let resources = self
            .resources
            .as_ref()
            .ok_or_else(|| TraceDecayError::Config {
                message: format!(
                    "branch sync failed: {cause}; production composition shut down before rollback"
                ),
            })?;
        if let Some(generation) = installed_generation {
            let _ = resources
                .invocation
                .code_index_schedulers
                .retire_serving_generation_if_exact(canonical_worktree_root, generation)
                .await;
        }
        if let Some(prepared) = prepared {
            crate::branch::rollback_prepared_branch_tracking(data_root, prepared).map_err(
                |rollback_error| TraceDecayError::Config {
                    message: format!(
                        "branch sync failed: {cause}; published branch rollback also failed: {rollback_error}"
                    ),
                },
            )?;
        }
        Ok(())
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
    scope: &tracedecay_application::ResolvedScope,
) -> Result<()> {
    timeout(Duration::from_secs(20), async {
        loop {
            // Scope-aware readiness is the authenticated demand boundary that
            // starts the registered route-local activation owner. The root-only
            // probe cannot mount an idle on-demand scheduler.
            if invocation
                .code_index_schedulers
                .latest_complete_ready_for_scope(scope)
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
    resources.store_administration.shutdown_session_sync().await;
    resources
        .store_administration
        .shutdown_host_admission_replay()
        .await;
    resources.invocation.shutdown().await;
    shutdown_detached_project_servers(
        tokio::time::Instant::now() + super::DAEMON_SHUTDOWN_DEADLINE,
        servers,
    )
    .await;
    match resources
        .store_administration
        .prepare_memory_graph_reconciliation_shutdown()
        .await
    {
        Ok(owner) => {
            owner.cancel();
            if let Err(error) = resources
                .store_administration
                .close_session_relation_graphs()
                .await
            {
                tracing::warn!(
                    event = "production_harness_graph_shutdown_failed",
                    error = %error,
                    "production-composition graph runtimes did not close cleanly"
                );
            }
            if let Err(error) = owner.shutdown().await {
                tracing::warn!(
                    event = "production_harness_graph_shutdown_failed",
                    error = %error,
                    "production-composition graph reconciliation tasks did not stop cleanly"
                );
            }
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
    async fn open_activates_code_index_before_waiting_for_publication() {
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
            let status = Command::new(crate::git::git_program())
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
        let resources = harness
            .resources
            .as_ref()
            .expect("production harness resources");
        assert!(
            resources
                .invocation
                .code_index_schedulers
                .latest_complete_ready(&project)
                .await
                .is_some(),
            "production harness returned before code-index publication"
        );
        harness.shutdown().await;
    }
}

#[cfg(test)]
mod generation_retention_test;

#[cfg(test)]
mod configuration_idempotency_journey_test;

#[cfg(test)]
mod semantic_activation_journey_test;

#[cfg(test)]
mod semantic_availability_journey_test;
