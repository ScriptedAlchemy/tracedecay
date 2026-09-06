//! Exact retained-memory target selection for one admitted profile.

use std::path::Path;
#[cfg(feature = "hotpath")]
use std::sync::atomic::{AtomicU64, Ordering};

use tracedecay_application::retained_surfaces::{MemoryScopeV1, RetainedProjectSelectorV1};
use tracedecay_domain::{FactOwnerV1, ProjectId};
use tracedecay_store::StoreShardScopeV1;

use super::map_execution_error;
use crate::tracedecay::TraceDecay;
use tracedecay_application::RetainedSurfaceExecutionErrorV1;
use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::store::memory::ProjectMemoryDbHandle;
use tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MemoryTargetAccessV1 {
    Read,
    Write,
}

pub(crate) struct RetainedMemoryTargetV1<'a> {
    database: ProjectMemoryDbHandle<'a>,
    owner: FactOwnerV1,
    // Ties the open-target gauge to the exact handle lifetime so cancelled or
    // failed requests cannot leak an "open" entry.
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

    pub(crate) fn database(&self) -> &Database {
        self.database.as_db()
    }

    pub(crate) fn owner(&self) -> &FactOwnerV1 {
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
pub(crate) async fn open_project_retained_memory_target<'a>(
    cg: &'a TraceDecay,
    registered_root: &Path,
    admitted_project_id: &ProjectId,
    memory_scope: Option<MemoryScopeV1>,
    selector: Option<&RetainedProjectSelectorV1>,
    access: MemoryTargetAccessV1,
) -> Result<RetainedMemoryTargetV1<'a>, RetainedSurfaceExecutionErrorV1> {
    if memory_scope == Some(MemoryScopeV1::User) {
        if selector.is_some() {
            return denied();
        }
        let database = open_profile_memory(cg.store_runtime_registry()).await?;
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
        if cg.project_root() != registered_root {
            return denied();
        }
        let owner = cg.project_memory_owner().map_err(map_execution_error)?;
        if owner
            != (FactOwnerV1::Project {
                project_id: admitted_project_id.clone(),
            })
        {
            return denied();
        }
        let database = cg.project_memory_db().await.map_err(map_execution_error)?;
        return Ok(RetainedMemoryTargetV1::new(database, owner));
    }
    if access == MemoryTargetAccessV1::Write {
        return denied();
    }
    open_selected_project_read_only(cg, selected_project_id).await
}

async fn open_profile_memory(
    registry: &DaemonSessionRuntimeRegistryV1,
) -> Result<Database, RetainedSurfaceExecutionErrorV1> {
    tracedecay_store_runtime::open_user_memory_db(registry)
        .await
        .map_err(map_execution_error)
}

#[hotpath::measure(label = "daemon.retained.memory.open_selected", future = true)]
async fn open_selected_project_read_only<'a>(
    cg: &TraceDecay,
    selected_project_id: &ProjectId,
) -> Result<RetainedMemoryTargetV1<'a>, RetainedSurfaceExecutionErrorV1> {
    let context = cg
        .profile_database()
        .project_registry_context_by_id(selected_project_id.as_str())
        .await
        .map_err(map_target_infrastructure_error)?
        .ok_or(RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized)?;
    if context.project.project_id.as_str() != selected_project_id.as_str() {
        return denied();
    }
    let roots = TraceDecay::enrolled_project_roots(
        TraceDecay::registry_context_candidate_roots(&context),
        selected_project_id,
    )
    .map_err(map_target_infrastructure_error)?;
    if roots.is_empty() {
        return denied();
    }
    let database = cg
        .store_runtime_registry()
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
    use std::path::Path;
    use std::sync::Arc;

    use super::*;

    fn open_options(profile_root: &Path) -> crate::tracedecay::TraceDecayOpenOptions {
        crate::tracedecay::TraceDecayOpenOptions {
            global_db_path: Some(profile_root.join("global.db")),
            profile_root: Some(profile_root.to_path_buf()),
        }
    }

    fn project_id(cg: &TraceDecay) -> ProjectId {
        let FactOwnerV1::Project { project_id } = cg.project_memory_owner().unwrap() else {
            panic!("fixture must have a project memory owner");
        };
        project_id
    }

    async fn register_project(cg: &TraceDecay, project_id: &ProjectId, project_root: &Path) {
        cg.profile_database()
            .upsert_code_project(project_id.as_str(), project_root, None, None, Some("main"))
            .await
            .unwrap();
    }

    async fn project_pair() -> (
        tempfile::TempDir,
        TraceDecay,
        TraceDecay,
        Arc<crate::host_admission::HostAdmissionTestRuntimeV1>,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        // `initialize_project_graph_for_test` canonicalizes the root it mounts,
        // so a selected id derived from the symlinked spelling registers a
        // second authority for the same typed project and the resolver refuses
        // it as `DuplicateProjectAuthority` (macOS `/var` -> `/private/var`).
        let base = tracedecay_runtime_core::lifecycle_lease::canonical_or_original(tmp.path());
        let profile_root = base.join("profile");
        let active_root = base.join("active");
        std::fs::create_dir_all(&active_root).unwrap();
        let active = TraceDecay::init_with_options(&active_root, open_options(&profile_root))
            .await
            .unwrap();
        let runtime = active.test_runtime_for_test().unwrap();
        let selected_root = base.join("selected");
        std::fs::create_dir_all(&selected_root).unwrap();
        let selected_id = ProjectId::new(
            tracedecay_runtime_core::storage::default_profile_project_id(&selected_root),
        )
        .unwrap();
        let sibling = Arc::new(
            runtime
                .sibling_project(&selected_root, selected_id)
                .await
                .unwrap(),
        );
        let selected = sibling
            .initialize_project_graph_for_test(&selected_root, open_options(&profile_root))
            .await
            .unwrap();
        for graph in [&active, &selected] {
            register_project(&active, &project_id(graph), graph.project_root()).await;
        }
        (tmp, active, selected, sibling)
    }

    fn selector(project_id: ProjectId) -> RetainedProjectSelectorV1 {
        RetainedProjectSelectorV1 { project_id }
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

    #[tokio::test]
    async fn selected_project_opens_its_exact_read_only_store_not_the_active_store() {
        let (_tmp, active, selected, _sibling) = project_pair().await;
        let active_id = project_id(&active);
        let selected_id = project_id(&selected);

        let active_target = open_project_retained_memory_target(
            &active,
            active.project_root(),
            &active_id,
            Some(MemoryScopeV1::Project),
            None,
            MemoryTargetAccessV1::Read,
        )
        .await
        .unwrap();
        let selected_selector = selector(selected_id.clone());
        let selected_target = open_project_retained_memory_target(
            &active,
            active.project_root(),
            &active_id,
            Some(MemoryScopeV1::Project),
            Some(&selected_selector),
            MemoryTargetAccessV1::Read,
        )
        .await
        .unwrap();

        assert!(active_target.database().is_writable());
        assert!(!selected_target.database().is_writable());
        assert_eq!(
            selected_target.owner(),
            &FactOwnerV1::Project {
                project_id: selected_id.clone(),
            }
        );
        assert!(!std::ptr::eq(
            active_target.database(),
            selected_target.database()
        ));
        assert!(matches!(
            &selected_target.database().registered_binding().shard_id.scope,
            StoreShardScopeV1::Project { project_id } if project_id == &selected_id
        ));
    }

    #[tokio::test]
    async fn missing_unenrolled_and_write_selected_targets_share_one_denial() {
        let (tmp, active, selected, _sibling) = project_pair().await;
        let active_id = project_id(&active);
        let selected_selector = selector(project_id(&selected));
        let missing_selector = selector(ProjectId::new("proj_missing").unwrap());
        let unenrolled_root = tmp.path().join("unenrolled");
        std::fs::create_dir_all(&unenrolled_root).unwrap();
        let unenrolled_id = ProjectId::new("proj_unenrolled").unwrap();
        register_project(&active, &unenrolled_id, &unenrolled_root).await;
        let unenrolled_selector = selector(unenrolled_id);

        for (selector, access) in [
            (&missing_selector, MemoryTargetAccessV1::Read),
            (&unenrolled_selector, MemoryTargetAccessV1::Read),
            (&selected_selector, MemoryTargetAccessV1::Write),
        ] {
            let error = open_project_retained_memory_target(
                &active,
                active.project_root(),
                &active_id,
                Some(MemoryScopeV1::Project),
                Some(selector),
                access,
            )
            .await
            .err()
            .expect("target must be denied");
            assert!(matches!(
                error,
                RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized
            ));
        }
    }
}
