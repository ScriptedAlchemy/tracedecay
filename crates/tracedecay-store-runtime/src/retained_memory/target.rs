//! Exact retained-memory target selection for one admitted profile.

use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(feature = "hotpath")]
use std::sync::atomic::{AtomicU64, Ordering};

use tracedecay_contracts::RetainedSurfaceExecutionErrorV1;
use tracedecay_contracts::retained_surfaces::{MemoryScopeV1, RetainedProjectSelectorV1};
use tracedecay_domain::{FactOwnerV1, ProjectId};
use tracedecay_global_db::{ProjectRegistryContext, RegisteredGlobalDbLeaseV1};
use tracedecay_runtime_core::db::Database;
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryTargetAccessV1 {
    Read,
    Write,
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
        if authority.project_root != registered_root {
            return denied();
        }
        let owner = FactOwnerV1::Project {
            project_id: admitted_project_id.clone(),
        };
        if authority.project_id != *admitted_project_id {
            return denied();
        }
        let database = authority
            .registry
            .mounted_project_memory(admitted_project_id)
            .map_err(map_execution_error)?;
        return Ok(RetainedMemoryTargetV1::new(
            ProjectMemoryDbHandle::Owned(Box::new(database)),
            owner,
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
    let roots = enrolled_project_roots(
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

fn registry_context_candidate_roots(context: &ProjectRegistryContext) -> Vec<PathBuf> {
    let mut candidates = vec![
        PathBuf::from(&context.project.canonical_root),
        PathBuf::from(&context.project.display_root),
    ];
    candidates.extend(
        context
            .aliases
            .iter()
            .map(|alias| PathBuf::from(&alias.alias_path)),
    );
    candidates
}

fn enrolled_project_roots(
    candidates: impl IntoIterator<Item = PathBuf>,
    project_id: &ProjectId,
) -> Result<Vec<PathBuf>, tracedecay_domain::errors::TraceDecayError> {
    let mut candidates = candidates.into_iter().collect::<Vec<_>>();
    candidates.sort();
    candidates.dedup();

    let mut roots = Vec::new();
    for candidate in candidates {
        let candidate = tracedecay_runtime_core::worktree::repository_identity_root(&candidate)
            .unwrap_or(candidate);
        let Ok(canonical) = candidate.canonicalize() else {
            continue;
        };
        if roots.contains(&canonical) {
            continue;
        }
        let named_id = match storage::read_repository_identity_marker(&canonical)? {
            Some(marker) => marker.project_id,
            None => storage::default_profile_project_id(&canonical),
        };
        if named_id == project_id.as_str() {
            roots.push(canonical);
        }
    }
    Ok(roots)
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
    use super::*;

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
