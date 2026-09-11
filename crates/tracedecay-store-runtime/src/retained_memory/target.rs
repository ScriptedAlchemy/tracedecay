//! Exact retained-memory target selection for one admitted profile.

use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(feature = "hotpath")]
use std::sync::atomic::{AtomicU64, Ordering};

use tracedecay_contracts::RetainedSurfaceExecutionErrorV1;
use tracedecay_contracts::retained_surfaces::{MemoryScopeV1, RetainedProjectSelectorV1};
use tracedecay_domain::{FactOwnerV1, ProjectId};
use tracedecay_global_db::{RegisteredGlobalDbLeaseV1, registry_context_candidate_roots};
use tracedecay_runtime_core::db::{Database, DatabaseAccessMode};
use tracedecay_runtime_core::storage;
use tracedecay_session_memory::fact_store::ProjectMemoryDbHandle;
use tracedecay_session_runtime::retained::map_execution_error;
use tracedecay_store::StoreShardScopeV1;

use crate::session_registry::{DaemonSessionRuntimeRegistryV1, open_user_memory_db};

#[derive(Clone)]
pub struct RetainedMemoryTargetAuthorityV1 {
    pub registry: Arc<DaemonSessionRuntimeRegistryV1>,
    pub profile_database: RegisteredGlobalDbLeaseV1,
    pub project_root: PathBuf,
    pub project_id: ProjectId,
    /// On-disk store identity from `StoreLayout.identity.project_id`.
    pub store_layout_project_id: ProjectId,
    /// Served project root from the live graph, not the admitted request root.
    pub served_project_root: PathBuf,
    /// Whether the served graph is published read-only.
    pub graph_read_only: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryTargetAccessV1 {
    Read,
    Write,
    /// Search records a retrieval projection, and both Search and Related
    /// reconcile the memory graph inline on read. Writable graphs take a
    /// write lease; a read-only graph or owner degrades to a read-only lease
    /// and reports `ReadOnly` telemetry instead of refusing.
    RecordRetrieval,
}

pub struct RetainedMemoryTargetV1<'a> {
    database: ProjectMemoryDbHandle<'a>,
    owner: FactOwnerV1,
    #[cfg(feature = "hotpath")]
    _observation: RetainedMemoryTargetObservationV1,
}

impl<'a> RetainedMemoryTargetV1<'a> {
    fn new(database: ProjectMemoryDbHandle<'a>, owner: FactOwnerV1) -> Self {
        Self {
            database,
            owner,
            #[cfg(feature = "hotpath")]
            _observation: RetainedMemoryTargetObservationV1::enter(),
        }
    }

    pub fn database(&self) -> &Database {
        self.database.as_db()
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
}

#[cfg(feature = "hotpath")]
static RETAINED_MEMORY_TARGETS_OPEN: AtomicU64 = AtomicU64::new(0);

#[cfg(feature = "hotpath")]
struct RetainedMemoryTargetObservationV1;

#[cfg(feature = "hotpath")]
impl RetainedMemoryTargetObservationV1 {
    fn enter() -> Self {
        let open = RETAINED_MEMORY_TARGETS_OPEN
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        hotpath::gauge!("daemon.retained.memory.target.opened_total").inc(1_u64);
        hotpath::gauge!("daemon.retained.memory.target.open").set(open);
        Self
    }
}

#[cfg(feature = "hotpath")]
impl Drop for RetainedMemoryTargetObservationV1 {
    fn drop(&mut self) {
        let _ = RETAINED_MEMORY_TARGETS_OPEN.fetch_update(
            Ordering::Relaxed,
            Ordering::Relaxed,
            |open| open.checked_sub(1),
        );
        hotpath::gauge!("daemon.retained.memory.target.open")
            .set(RETAINED_MEMORY_TARGETS_OPEN.load(Ordering::Relaxed));
    }
}

#[hotpath::measure(label = "daemon.retained.memory.open_target", future = true)]
pub async fn open_project_retained_memory_target(
    authority: &RetainedMemoryTargetAuthorityV1,
    registered_root: &Path,
    admitted_project_id: &ProjectId,
    memory_scope: Option<MemoryScopeV1>,
    selector: Option<&RetainedProjectSelectorV1>,
    access: MemoryTargetAccessV1,
) -> Result<RetainedMemoryTargetV1<'static>, RetainedSurfaceExecutionErrorV1> {
    if memory_scope == Some(MemoryScopeV1::User) {
        if selector.is_some() {
            return denied();
        }
        let database = open_profile_memory(&authority.registry).await?;
        return Ok(RetainedMemoryTargetV1::new(
            ProjectMemoryDbHandle::Owned(Box::new(database)),
            FactOwnerV1::Profile,
        ));
    }
    if memory_scope.is_some_and(|scope| scope != MemoryScopeV1::Project) {
        return denied();
    }
    let selected_project_id = selector.map_or(admitted_project_id, |value| &value.project_id);
    if selected_project_id == admitted_project_id {
        if authority.project_id != *admitted_project_id {
            return denied();
        }
        if authority.served_project_root != registered_root {
            return denied();
        }
        if authority.store_layout_project_id != *admitted_project_id {
            return denied();
        }
        if access == MemoryTargetAccessV1::Write && authority.graph_read_only {
            return denied();
        }
        let database = match access {
            MemoryTargetAccessV1::RecordRetrieval => authority
                .registry
                .mounted_project_memory_recording(admitted_project_id, authority.graph_read_only)
                .map_err(map_execution_error)?,
            MemoryTargetAccessV1::Read => authority
                .registry
                .mounted_project_memory(admitted_project_id, DatabaseAccessMode::ReadOnly)
                .map_err(map_execution_error)?,
            MemoryTargetAccessV1::Write => authority
                .registry
                .mounted_project_memory(admitted_project_id, DatabaseAccessMode::ReadWrite)
                .map_err(map_execution_error)?,
        };
        if access == MemoryTargetAccessV1::Write && !database.is_writable() {
            return denied();
        }
        return Ok(RetainedMemoryTargetV1::new(
            ProjectMemoryDbHandle::Owned(Box::new(database)),
            FactOwnerV1::Project {
                project_id: authority.store_layout_project_id.clone(),
            },
        ));
    }
    if access == MemoryTargetAccessV1::Write {
        return denied();
    }
    open_selected_project_read_only(authority, selected_project_id).await
}

async fn open_profile_memory(
    registry: &DaemonSessionRuntimeRegistryV1,
) -> Result<Database, RetainedSurfaceExecutionErrorV1> {
    open_user_memory_db(registry)
        .await
        .map_err(map_execution_error)
}

#[hotpath::measure(label = "daemon.retained.memory.open_selected", future = true)]
async fn open_selected_project_read_only(
    authority: &RetainedMemoryTargetAuthorityV1,
    selected_project_id: &ProjectId,
) -> Result<RetainedMemoryTargetV1<'static>, RetainedSurfaceExecutionErrorV1> {
    let context = authority
        .profile_database
        .project_registry_context_by_id(selected_project_id.as_str())
        .await
        .map_err(map_target_infrastructure_error)?
        .ok_or(RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized)?;
    if context.project.project_id.as_str() != selected_project_id.as_str() {
        return denied();
    }
    let roots = storage::enrolled_project_roots(
        registry_context_candidate_roots(&context),
        selected_project_id,
    )
    .map_err(map_target_infrastructure_error)?;
    if roots.is_empty() {
        return denied();
    }
    let database = authority
        .registry
        .project_memory_read_only(selected_project_id.clone(), roots)
        .await
        .map_err(map_target_infrastructure_error)?;
    let exact_scope = matches!(
        &database.registered_binding().shard_id.scope,
        StoreShardScopeV1::Project { project_id } if project_id == selected_project_id
    );
    if database.is_writable() || !exact_scope {
        return denied();
    }
    Ok(RetainedMemoryTargetV1::new(
        ProjectMemoryDbHandle::Owned(Box::new(database)),
        FactOwnerV1::Project {
            project_id: selected_project_id.clone(),
        },
    ))
}

fn denied<T>() -> Result<T, RetainedSurfaceExecutionErrorV1> {
    Err(RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized)
}

fn map_target_infrastructure_error(
    error: tracedecay_domain::errors::TraceDecayError,
) -> RetainedSurfaceExecutionErrorV1 {
    match error {
        tracedecay_domain::errors::TraceDecayError::ProfileResetRequired { .. } => {
            RetainedSurfaceExecutionErrorV1::ProfileResetRequired
        }
        tracedecay_domain::errors::TraceDecayError::ResetRequired { .. } => {
            RetainedSurfaceExecutionErrorV1::ProjectResetRequired
        }
        error => RetainedSurfaceExecutionErrorV1::unavailable(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::TempDir;
    use tracedecay_daemon_identity::profile_identity;

    use super::*;

    struct MemoryTargetFixture {
        registry: Arc<DaemonSessionRuntimeRegistryV1>,
        profile_database: RegisteredGlobalDbLeaseV1,
        project_id: ProjectId,
        project_root: PathBuf,
        profile_root: PathBuf,
        _database_scope: tracedecay_runtime_core::db::DaemonDatabaseScope,
        _temp: TempDir,
    }

    impl MemoryTargetFixture {
        async fn new(label: &str) -> Self {
            let temp = TempDir::new().expect("memory target fixture root");
            let profile_root = temp.path().join("profile");
            let identity =
                profile_identity::load_or_create(&profile_root).expect("profile identity");
            let database_scope =
                tracedecay_runtime_core::db::enter_daemon_database_scope(&profile_root, 29, label)
                    .expect("daemon database scope");
            let registry = Arc::new(
                DaemonSessionRuntimeRegistryV1::open(identity)
                    .await
                    .expect("session runtime registry"),
            );
            let project_id =
                ProjectId::new(format!("project.retained-memory.{label}")).expect("project id");
            let project_root = temp.path().join("served");
            std::fs::create_dir_all(&project_root).expect("served project root");
            tracedecay_runtime_core::storage::pin_fixture_repository_identity(
                &project_root,
                project_id.as_str(),
            )
            .expect("project enrollment");
            let _mounted = registry
                .project_memory(project_id.clone(), [project_root.clone()])
                .await
                .expect("mounted project memory");
            let profile_database = registry.profile_database().await.expect("profile database");
            Self {
                registry,
                profile_database,
                project_id,
                project_root,
                profile_root,
                _database_scope: database_scope,
                _temp: temp,
            }
        }

        async fn reopen_read_only_owner(self) -> Self {
            let identity =
                profile_identity::load_or_create(&self.profile_root).expect("profile identity");
            let project_id = self.project_id.clone();
            let project_root = self.project_root.clone();
            let profile_root = self.profile_root.clone();
            let temp = self._temp;
            self.registry
                .shutdown_memory_graph_reconciliation_tasks()
                .await
                .expect("join seed graph reconciliation");
            drop((self.registry, self.profile_database, self._database_scope));
            let database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
                &profile_root,
                30,
                "read-only-reopen",
            )
            .expect("reopened daemon database scope");
            let registry = Arc::new(
                DaemonSessionRuntimeRegistryV1::open(identity)
                    .await
                    .expect("reopened session runtime registry"),
            );
            let _mounted = registry
                .publish_read_only_memory_owner_for_test(project_id.clone(), [project_root.clone()])
                .await
                .expect("mounted read-only project memory");
            let profile_database = registry.profile_database().await.expect("profile database");
            Self {
                registry,
                profile_database,
                project_id,
                project_root,
                profile_root,
                _database_scope: database_scope,
                _temp: temp,
            }
        }

        fn authority(
            &self,
            store_layout_project_id: ProjectId,
            served_project_root: PathBuf,
        ) -> RetainedMemoryTargetAuthorityV1 {
            RetainedMemoryTargetAuthorityV1 {
                registry: Arc::clone(&self.registry),
                profile_database: self.profile_database.clone(),
                project_root: self.project_root.clone(),
                project_id: self.project_id.clone(),
                store_layout_project_id,
                served_project_root,
                graph_read_only: false,
            }
        }
    }

    async fn open_same_project(
        authority: &RetainedMemoryTargetAuthorityV1,
        registered_root: &Path,
        admitted_project_id: &ProjectId,
    ) -> Result<RetainedMemoryTargetV1<'static>, RetainedSurfaceExecutionErrorV1> {
        open_same_project_with(
            authority,
            registered_root,
            admitted_project_id,
            MemoryTargetAccessV1::Read,
        )
        .await
    }

    #[tokio::test]
    async fn same_project_open_denies_when_store_layout_identity_disagrees() {
        let fixture = MemoryTargetFixture::new("store-id-drift").await;
        let foreign = ProjectId::new("project.retained-memory.foreign-store").expect("foreign id");
        let authority = fixture.authority(foreign, fixture.project_root.clone());
        let error = open_same_project(&authority, &fixture.project_root, &fixture.project_id)
            .await
            .err()
            .expect("store identity drift must deny");
        assert!(matches!(
            error,
            RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized
        ));
    }

    #[tokio::test]
    async fn same_project_open_denies_when_mounted_scope_disagrees() {
        let fixture = MemoryTargetFixture::new("mounted-scope-drift").await;
        let foreign = ProjectId::new("project.retained-memory.foreign-mount").expect("foreign id");
        let mut authority =
            fixture.authority(fixture.project_id.clone(), fixture.project_root.clone());
        authority.project_id = foreign;
        let error = open_same_project(&authority, &fixture.project_root, &fixture.project_id)
            .await
            .err()
            .expect("mounted scope drift must deny");
        assert!(matches!(
            error,
            RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized
        ));
    }

    #[tokio::test]
    async fn same_project_read_open_issues_a_read_only_lease() {
        let fixture = MemoryTargetFixture::new("read-lease").await;
        let authority = fixture.authority(fixture.project_id.clone(), fixture.project_root.clone());
        let target = open_same_project(&authority, &fixture.project_root, &fixture.project_id)
            .await
            .expect("matching store identity must open");
        assert!(
            !target.database().is_writable(),
            "Read access must issue a read-only lease"
        );
    }

    async fn open_same_project_with(
        authority: &RetainedMemoryTargetAuthorityV1,
        registered_root: &Path,
        admitted_project_id: &ProjectId,
        access: MemoryTargetAccessV1,
    ) -> Result<RetainedMemoryTargetV1<'static>, RetainedSurfaceExecutionErrorV1> {
        open_project_retained_memory_target(
            authority,
            registered_root,
            admitted_project_id,
            Some(MemoryScopeV1::Project),
            None,
            access,
        )
        .await
    }

    #[tokio::test]
    async fn same_project_record_retrieval_open_issues_a_writable_lease() {
        let fixture = MemoryTargetFixture::new("retrieval-lease").await;
        let authority = fixture.authority(fixture.project_id.clone(), fixture.project_root.clone());
        let target = open_same_project_with(
            &authority,
            &fixture.project_root,
            &fixture.project_id,
            MemoryTargetAccessV1::RecordRetrieval,
        )
        .await
        .expect("matching store identity must open");
        assert!(
            target.database().is_writable(),
            "RecordRetrieval on a writable graph must issue a write lease"
        );
    }

    #[tokio::test]
    async fn same_project_record_retrieval_degrades_when_owner_is_not_writable() {
        let fixture = MemoryTargetFixture::new("retrieval-owner-ro")
            .await
            .reopen_read_only_owner()
            .await;
        let authority = fixture.authority(fixture.project_id.clone(), fixture.project_root.clone());
        assert!(!authority.graph_read_only);
        let target = open_same_project_with(
            &authority,
            &fixture.project_root,
            &fixture.project_id,
            MemoryTargetAccessV1::RecordRetrieval,
        )
        .await
        .expect("owner-not-writable search must degrade, not fail");
        assert!(
            !target.database().is_writable(),
            "RecordRetrieval must fall back to a read-only lease when the owner refuses writability"
        );
    }

    #[tokio::test]
    async fn same_project_record_retrieval_on_read_only_graph_issues_a_read_only_lease() {
        let fixture = MemoryTargetFixture::new("retrieval-readonly").await;
        let mut authority =
            fixture.authority(fixture.project_id.clone(), fixture.project_root.clone());
        authority.graph_read_only = true;
        let target = open_same_project_with(
            &authority,
            &fixture.project_root,
            &fixture.project_id,
            MemoryTargetAccessV1::RecordRetrieval,
        )
        .await
        .expect("read-only graph must degrade retrieval recording, not deny");
        assert!(
            !target.database().is_writable(),
            "RecordRetrieval on a read-only graph must issue a read-only lease"
        );
    }

    #[tokio::test]
    async fn same_project_write_against_read_only_graph_is_denied() {
        let fixture = MemoryTargetFixture::new("write-readonly").await;
        let mut authority =
            fixture.authority(fixture.project_id.clone(), fixture.project_root.clone());
        authority.graph_read_only = true;
        let error = open_project_retained_memory_target(
            &authority,
            &fixture.project_root,
            &fixture.project_id,
            Some(MemoryScopeV1::Project),
            None,
            MemoryTargetAccessV1::Write,
        )
        .await
        .err()
        .expect("write against a read-only graph must deny");
        assert!(matches!(
            error,
            RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized
        ));
    }

    #[tokio::test]
    async fn same_project_open_denies_when_served_root_disagrees() {
        let fixture = MemoryTargetFixture::new("served-root-drift").await;
        let foreign_root = fixture._temp.path().join("foreign-served");
        std::fs::create_dir_all(&foreign_root).expect("foreign served root");
        let authority = fixture.authority(fixture.project_id.clone(), foreign_root);
        let error = open_same_project(&authority, &fixture.project_root, &fixture.project_id)
            .await
            .err()
            .expect("served root drift must deny");
        assert!(matches!(
            error,
            RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized
        ));
    }

    #[test]
    fn selected_target_infrastructure_failures_remain_typed() {
        let RetainedSurfaceExecutionErrorV1::Unavailable { detail } =
            map_target_infrastructure_error(tracedecay_domain::errors::TraceDecayError::Config {
                message: "corrupt registry".to_owned(),
            })
        else {
            panic!("infrastructure failures must map to the unavailable terminal");
        };
        assert!(
            detail.contains("corrupt registry"),
            "the detail must carry the underlying cause, got: {detail}"
        );
        assert!(matches!(
            map_target_infrastructure_error(
                tracedecay_domain::errors::TraceDecayError::ProfileResetRequired {
                    component: "profile-memory",
                    found_version: Some(1),
                    required_version: 2,
                }
            ),
            RetainedSurfaceExecutionErrorV1::ProfileResetRequired
        ));
        assert!(matches!(
            map_target_infrastructure_error(
                tracedecay_domain::errors::TraceDecayError::reset_required(
                    "project-memory",
                    "schema mismatch",
                )
            ),
            RetainedSurfaceExecutionErrorV1::ProjectResetRequired
        ));
    }
}
