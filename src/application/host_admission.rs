//! Root composition for host-admission test runtimes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use tracedecay_usecases::host_admission::*;

use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::global_db::RegisteredGlobalDb;
use crate::tracedecay::{TraceDecay, TraceDecayOpenOptions};
use tracedecay_domain::{BrainId, ProjectId, UserProfileId};
use tracedecay_runtime_core::db::DaemonDatabaseScope;
use tracedecay_runtime_core::errors::{Result, TraceDecayError};
use tracedecay_store::StoreShardScopeV1;

type StorageTestRuntime = tracedecay_global_db::tests::harness::HostAdmissionTestRuntimeV1;

/// Registered host-admission fixture assembled by the composition root.
///
/// The lower storage fixture exposes database-scoped test helpers. This
/// wrapper retains the canonical daemon scope and session-runtime registry
/// needed by graph, daemon, MCP, and hook integration tests.
#[doc(hidden)]
pub struct HostAdmissionTestRuntimeV1 {
    storage: StorageTestRuntime,
    brain_id: BrainId,
    profile_id: UserProfileId,
    profile_root: PathBuf,
    project_id: Option<ProjectId>,
    profile_database: Arc<RegisteredGlobalDb>,
    profile_registered: Arc<RegisteredGlobalDb>,
    project_registered: Option<Arc<RegisteredGlobalDb>>,
    session_registry: Arc<DaemonSessionRuntimeRegistryV1>,
    _database_scope: DaemonDatabaseScope,
}

impl HostAdmissionTestRuntimeV1 {
    #[doc(hidden)]
    pub async fn profile(profile_root: impl AsRef<Path>) -> Result<Self> {
        Self::open(profile_root.as_ref().to_path_buf(), None).await
    }

    #[doc(hidden)]
    pub async fn project(
        profile_root: impl AsRef<Path>,
        project_root: impl AsRef<Path>,
        project_id: ProjectId,
    ) -> Result<Self> {
        Self::open(
            profile_root.as_ref().to_path_buf(),
            Some((project_root.as_ref().to_path_buf(), project_id)),
        )
        .await
    }

    /// [`Self::project`] returning proof that project authorities are mounted.
    #[doc(hidden)]
    pub async fn project_scoped(
        profile_root: impl AsRef<Path>,
        project_root: impl AsRef<Path>,
        project_id: ProjectId,
    ) -> Result<ProjectScopedTestRuntimeV1> {
        ProjectScopedTestRuntimeV1::new(
            Self::project(profile_root, project_root, project_id).await?,
        )
    }

    async fn open(profile_root: PathBuf, project: Option<(PathBuf, ProjectId)>) -> Result<Self> {
        prepare_host_admission_test_profile_root(&profile_root)?;
        if let Some((project_root, project_id)) = project.as_ref() {
            prepare_host_admission_test_project_root(project_root, project_id)?;
        }

        let identity = crate::daemon::profile_identity::load_or_create(&profile_root)?;
        let database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
            identity.profile_root(),
            1,
            "host-admission-test-runtime",
        )?;
        let session_registry =
            Arc::new(DaemonSessionRuntimeRegistryV1::open(identity.clone()).await?);
        let profile_database = session_registry.profile_database().await?;
        let profile_registered = session_registry.profile_sessions().await?;
        let (project_id, project_registered) = if let Some((project_root, project_id)) = project {
            let registered = session_registry
                .project_sessions(project_id.clone(), [project_root])
                .await?;
            (Some(project_id), Some(registered))
        } else {
            (None, None)
        };
        validate_registered_authorities(
            identity.brain_id(),
            identity.profile_id(),
            project_id.as_ref(),
            profile_database.as_ref(),
            profile_registered.as_ref(),
            project_registered.as_deref(),
        )?;
        let storage = StorageTestRuntime::from_registered_databases_for_test(
            Arc::clone(&profile_database),
            Arc::clone(&profile_registered),
            project_registered.clone(),
        );

        Ok(Self {
            storage,
            brain_id: identity.brain_id().clone(),
            profile_id: identity.profile_id().clone(),
            profile_root,
            project_id,
            profile_database,
            profile_registered,
            project_registered,
            session_registry,
            _database_scope: database_scope,
        })
    }

    #[doc(hidden)]
    pub fn canonical_project_key(project_path: &Path) -> String {
        StorageTestRuntime::canonical_project_key(project_path)
    }

    #[doc(hidden)]
    pub fn profile_root_for_test(&self) -> &Path {
        &self.profile_root
    }

    pub fn facade(&self) -> HostAdmissionFacade<'_> {
        match (self.project_id.as_ref(), self.project_registered.as_ref()) {
            (Some(project_id), Some(project_registered)) => HostAdmissionFacade::new(
                HostAdmissionAuthorities::registered_for_project(
                    self.brain_id.clone(),
                    self.profile_id.clone(),
                    project_id.clone(),
                    project_registered,
                )
                .with_profile_registered(self.profile_id.clone(), self.profile_registered.as_ref()),
            ),
            _ => HostAdmissionFacade::new(HostAdmissionAuthorities::for_profile(
                self.brain_id.clone(),
                self.profile_id.clone(),
                self.profile_registered.as_ref(),
            )),
        }
    }

    /// Initializes a project graph through this retained registered runtime.
    #[doc(hidden)]
    pub async fn initialize_project_graph_for_test(
        &self,
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
    ) -> Result<TraceDecay> {
        let project_id = self
            .project_id
            .as_ref()
            .ok_or_else(|| TraceDecayError::Config {
                message: "project graph initialization requires project-scoped test authority"
                    .to_owned(),
            })?;
        let project_database =
            self.project_registered
                .clone()
                .ok_or_else(|| TraceDecayError::Config {
                    message: "project graph initialization requires a registered project session"
                        .to_owned(),
                })?;
        let store_layout = TraceDecay::resolve_registered_configuration_layout(
            project_root,
            &open_options,
            self.profile_database.as_ref(),
            true,
        )
        .await?;
        if store_layout.identity.project_id.as_deref() != Some(project_id.as_str()) {
            return Err(TraceDecayError::Config {
                message: "project graph identity differs from registered test authority".to_owned(),
            });
        }
        TraceDecay::init_with_registered_configuration(
            project_root,
            open_options,
            store_layout,
            project_database,
            Arc::clone(&self.profile_database),
            Arc::clone(&self.session_registry),
        )
        .await
    }

    /// Reopens an existing project graph through this retained runtime.
    #[doc(hidden)]
    pub async fn open_project_graph_for_test(
        &self,
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
    ) -> Result<TraceDecay> {
        let (store_layout, project_database) = self
            .registered_project_open_inputs(project_root, &open_options)
            .await?;
        TraceDecay::open_with_registered_configuration(
            project_root,
            open_options,
            store_layout,
            project_database,
            Arc::clone(&self.profile_database),
            Arc::clone(&self.session_registry),
        )
        .await
    }

    /// Opens one tracked branch through this retained registered runtime.
    #[doc(hidden)]
    pub async fn open_project_branch_for_test(
        &self,
        project_root: &Path,
        branch_name: &str,
        open_options: TraceDecayOpenOptions,
    ) -> Result<TraceDecay> {
        let project_id = self
            .project_id
            .as_ref()
            .ok_or_else(|| TraceDecayError::Config {
                message: "project branch open requires project-scoped test authority".to_owned(),
            })?;
        let project_database =
            self.project_registered
                .clone()
                .ok_or_else(|| TraceDecayError::Config {
                    message: "project branch open requires a registered project session".to_owned(),
                })?;
        let store_layout = TraceDecay::resolve_registered_configuration_layout(
            project_root,
            &open_options,
            self.profile_database.as_ref(),
            true,
        )
        .await?;
        if store_layout.identity.project_id.as_deref() != Some(project_id.as_str()) {
            return Err(TraceDecayError::Config {
                message: "project branch identity differs from registered test authority"
                    .to_owned(),
            });
        }
        TraceDecay::open_branch_with_registered_configuration(
            project_root,
            branch_name,
            open_options,
            store_layout,
            project_database,
            Arc::clone(&self.profile_database),
            Arc::clone(&self.session_registry),
        )
        .await
    }

    /// Reopens an existing graph read-only without inferring authority.
    #[doc(hidden)]
    pub async fn open_project_graph_read_only_for_test(
        &self,
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
    ) -> Result<TraceDecay> {
        let (store_layout, project_database) = self
            .registered_project_open_inputs(project_root, &open_options)
            .await?;
        TraceDecay::open_read_only_with_registered_configuration(
            project_root,
            open_options,
            store_layout,
            project_database,
            Arc::clone(&self.profile_database),
            Arc::clone(&self.session_registry),
        )
        .await
    }

    async fn registered_project_open_inputs(
        &self,
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
    ) -> Result<(
        tracedecay_runtime_core::storage::StoreLayout,
        Arc<RegisteredGlobalDb>,
    )> {
        let project_id = self
            .project_id
            .as_ref()
            .ok_or_else(|| TraceDecayError::Config {
                message: "project graph open requires project-scoped test authority".to_owned(),
            })?;
        let project_database =
            self.project_registered
                .clone()
                .ok_or_else(|| TraceDecayError::Config {
                    message: "project graph open requires a registered project session".to_owned(),
                })?;
        let store_layout = TraceDecay::resolve_registered_configuration_layout(
            project_root,
            open_options,
            self.profile_database.as_ref(),
            true,
        )
        .await?;
        if store_layout.identity.project_id.as_deref() != Some(project_id.as_str()) {
            return Err(TraceDecayError::Config {
                message: "project graph identity differs from registered test authority".to_owned(),
            });
        }
        Ok((store_layout, project_database))
    }
}

impl std::ops::Deref for HostAdmissionTestRuntimeV1 {
    type Target = tracedecay_global_db::tests::harness::HostAdmissionTestRuntimeV1;

    fn deref(&self) -> &Self::Target {
        &self.storage
    }
}

/// A root test runtime statically known to carry project authority.
#[doc(hidden)]
#[derive(Clone)]
pub struct ProjectScopedTestRuntimeV1(Arc<HostAdmissionTestRuntimeV1>);

impl ProjectScopedTestRuntimeV1 {
    #[doc(hidden)]
    pub fn new(runtime: impl Into<Arc<HostAdmissionTestRuntimeV1>>) -> Result<Self> {
        let runtime = runtime.into();
        if runtime.project_id.is_none() || runtime.project_registered.is_none() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "test runtime for profile '{}' is profile-scoped; project-scoped authority \
                     requires HostAdmissionTestRuntimeV1::project",
                    runtime.profile_root.display()
                ),
            });
        }
        Ok(Self(runtime))
    }

    #[doc(hidden)]
    pub fn into_runtime(self) -> Arc<HostAdmissionTestRuntimeV1> {
        self.0
    }
}

impl std::ops::Deref for ProjectScopedTestRuntimeV1 {
    type Target = HostAdmissionTestRuntimeV1;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

fn validate_registered_authorities(
    brain_id: &BrainId,
    profile_id: &UserProfileId,
    project_id: Option<&ProjectId>,
    profile_database: &RegisteredGlobalDb,
    profile_registered: &RegisteredGlobalDb,
    project_registered: Option<&RegisteredGlobalDb>,
) -> Result<()> {
    let profile_shard = &profile_database.binding().shard_id;
    let profile_sessions_shard = &profile_registered.binding().shard_id;
    let profile_identity_matches = &profile_shard.brain_id == brain_id
        && &profile_shard.profile_id == profile_id
        && profile_shard.scope == StoreShardScopeV1::Profile;
    let profile_sessions_identity_matches = &profile_sessions_shard.brain_id == brain_id
        && &profile_sessions_shard.profile_id == profile_id
        && profile_sessions_shard.scope == StoreShardScopeV1::ProfileSessions;
    let project_identity_matches = match (project_id, project_registered) {
        (None, None) => true,
        (Some(project_id), Some(project_registered)) => {
            let shard = &project_registered.binding().shard_id;
            &shard.brain_id == brain_id
                && &shard.profile_id == profile_id
                && matches!(
                    &shard.scope,
                    StoreShardScopeV1::ProjectSessions {
                        project_id: shard_project_id
                    } if shard_project_id == project_id
                )
        }
        _ => false,
    };
    if profile_identity_matches && profile_sessions_identity_matches && project_identity_matches {
        return Ok(());
    }
    Err(TraceDecayError::Config {
        message: "registered test databases differ from the retained profile/project authority"
            .to_owned(),
    })
}

#[cfg(unix)]
fn prepare_host_admission_test_profile_root(profile_root: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(profile_root).map_err(|error| TraceDecayError::Config {
        message: format!(
            "failed to create host-admission test profile '{}': {error}",
            profile_root.display()
        ),
    })?;
    let metadata =
        std::fs::symlink_metadata(profile_root).map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to inspect host-admission test profile '{}': {error}",
                profile_root.display()
            ),
        })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(TraceDecayError::Config {
            message: format!(
                "host-admission test profile '{}' must be a regular directory",
                profile_root.display()
            ),
        });
    }
    std::fs::set_permissions(profile_root, std::fs::Permissions::from_mode(0o700)).map_err(
        |error| TraceDecayError::Config {
            message: format!(
                "failed to restrict host-admission test profile '{}': {error}",
                profile_root.display()
            ),
        },
    )
}

#[cfg(not(unix))]
fn prepare_host_admission_test_profile_root(profile_root: &Path) -> Result<()> {
    std::fs::create_dir_all(profile_root).map_err(|error| TraceDecayError::Config {
        message: format!(
            "failed to create host-admission test profile '{}': {error}",
            profile_root.display()
        ),
    })
}

fn prepare_host_admission_test_project_root(
    project_root: &Path,
    project_id: &ProjectId,
) -> Result<()> {
    std::fs::create_dir_all(project_root).map_err(|error| TraceDecayError::Config {
        message: format!(
            "failed to create host-admission test project '{}': {error}",
            project_root.display()
        ),
    })?;
    if tracedecay_runtime_core::storage::read_enrollment_marker(project_root)?.is_none() {
        tracedecay_runtime_core::storage::write_enrollment_marker(
            project_root,
            &tracedecay_runtime_core::storage::EnrollmentMarker {
                project_id: project_id.as_str().to_owned(),
                storage_mode: tracedecay_runtime_core::storage::StorageMode::ProfileSharded,
            },
        )?;
    }
    Ok(())
}
