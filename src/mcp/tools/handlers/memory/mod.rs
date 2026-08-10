//! Cross-session and holographic memory handlers.

use std::path::{Path, PathBuf};

use serde_json::Value;
use tracedecay_domain::{FactOwnerV1, ProjectId};

use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::db::Database;
use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::memory::user::open_user_memory_db;
use crate::store::DatabaseFactStore;
use crate::store::memory::ProjectMemoryDbHandle;
use crate::tracedecay::TraceDecay;
use tracedecay_usecases::memory::{MemoryApplication, MemoryApplicationError};

use super::support::{
    profile_root_for_global_db, project_registry_context, project_selector_present,
};

mod registered_target;

use registered_target::open_registered_project_memory_read_only;

pub(super) struct TargetMemoryDb<'a> {
    db: ProjectMemoryDbHandle<'a>,
    pub(super) project_root: PathBuf,
    owner: FactOwnerV1,
}

impl TargetMemoryDb<'_> {
    fn db(&self) -> &Database {
        self.db.as_db()
    }

    pub(super) fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
}

async fn open_user_memory_target(
    registry: &DaemonSessionRuntimeRegistryV1,
    profile_root: &Path,
) -> Result<TargetMemoryDb<'static>> {
    Ok(TargetMemoryDb {
        db: ProjectMemoryDbHandle::Owned(Box::new(open_user_memory_db(registry).await?)),
        project_root: profile_root.to_path_buf(),
        owner: FactOwnerV1::Profile,
    })
}

fn requests_user_memory(args: &Value) -> bool {
    args.get("memory_scope").and_then(Value::as_str) == Some("user")
}

fn project_memory_owner(project_id: &str) -> Result<FactOwnerV1> {
    let project_id = ProjectId::new(project_id.to_owned())
        .map_err(|error| config_error(format!("invalid project memory owner: {error}")))?;
    Ok(FactOwnerV1::Project { project_id })
}

fn active_project_memory_owner(cg: &TraceDecay) -> Result<FactOwnerV1> {
    let project_id = cg
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .ok_or_else(|| config_error("active project has no authoritative project_id"))?;
    project_memory_owner(project_id)
}

pub(super) async fn open_target_memory_db<'a>(
    cg: &'a TraceDecay,
    args: &Value,
    global_db: Option<&RegisteredGlobalDb>,
) -> Result<TargetMemoryDb<'a>> {
    if requests_user_memory(args) {
        if project_selector_present(args, &["project_path"]) {
            return Err(config_error(
                "memory_scope=user cannot be combined with a project selector",
            ));
        }
        let profile_root = profile_root_for_global_db(global_db)?;
        return open_user_memory_target(cg.store_runtime_registry(), &profile_root).await;
    }
    let Some(context) = project_registry_context(args, &["project_path"], global_db).await? else {
        return Ok(TargetMemoryDb {
            db: cg.project_memory_db().await?,
            project_root: cg.project_root().to_path_buf(),
            owner: active_project_memory_owner(cg)?,
        });
    };
    let selected_project_id = context.project.project_id.as_str();
    // The selector may name the project this instance already serves — by id,
    // by an alias, or through a branch shard. That is the active project's own
    // memory, so resolve it through the active resolver, which routes a
    // branch-serving instance back to the shared project store.
    if cg.store_layout().identity.project_id.as_deref() == Some(selected_project_id) {
        return Ok(TargetMemoryDb {
            db: cg.project_memory_db().await?,
            project_root: cg.project_root().to_path_buf(),
            owner: project_memory_owner(selected_project_id)?,
        });
    }
    let owner = project_memory_owner(selected_project_id)?;
    let db = open_registered_project_memory_read_only(cg, &context).await?;
    Ok(TargetMemoryDb {
        db: ProjectMemoryDbHandle::Owned(Box::new(db)),
        project_root: PathBuf::from(&context.project.display_root),
        owner,
    })
}

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

fn memory_application_error(error: MemoryApplicationError) -> TraceDecayError {
    TraceDecayError::database_operation("memory application", error)
}

pub(super) fn memory_application<'a>(
    target_memory: &'a TargetMemoryDb<'_>,
) -> Result<MemoryApplication<DatabaseFactStore<'a>>> {
    MemoryApplication::new(
        target_memory.owner().clone(),
        DatabaseFactStore::new(target_memory.db()),
    )
    .map_err(memory_application_error)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::memory::types::{AddFactRequest, MemoryCategory};
    use tracedecay_usecases::memory::MemoryOperationContext;

    fn fact(content: &str) -> AddFactRequest {
        AddFactRequest {
            content: content.to_owned(),
            category: MemoryCategory::General,
            source: None,
            tags: Vec::new(),
            entities: Vec::new(),
            trust: None,
            metadata: json!({}),
        }
    }

    fn open_options(profile_root: &Path) -> crate::tracedecay::TraceDecayOpenOptions {
        crate::tracedecay::TraceDecayOpenOptions {
            global_db_path: Some(profile_root.join("global.db")),
            profile_root: Some(profile_root.to_path_buf()),
        }
    }

    async fn register_project(cg: &TraceDecay, project_id: &str, project_root: &Path) {
        cg.profile_database()
            .upsert_code_project(project_id, project_root, None, None, Some("main"))
            .await
            .expect("registry must admit the fixture project root");
    }

    fn project_id_of(cg: &TraceDecay) -> String {
        cg.store_layout()
            .identity
            .project_id
            .clone()
            .expect("fixture graph must carry an authoritative project identity")
    }

    async fn cross_project_memory_pair() -> (
        tempfile::TempDir,
        TraceDecay,
        TraceDecay,
        std::sync::Arc<crate::host_admission::HostAdmissionTestRuntimeV1>,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let profile_root = tmp.path().join("profile");
        let active_root = tmp.path().join("active");
        std::fs::create_dir_all(&active_root).unwrap();
        let active = TraceDecay::init_with_options(&active_root, open_options(&profile_root))
            .await
            .unwrap();
        let runtime = active
            .test_runtime_for_test()
            .expect("standalone fixture runtime");
        let target_root = tmp.path().join("target");
        std::fs::create_dir_all(&target_root).unwrap();
        let target_project_id =
            ProjectId::new(crate::storage::default_profile_project_id(&target_root))
                .expect("typed target project identity");
        let sibling = std::sync::Arc::new(
            runtime
                .sibling_project(&target_root, target_project_id)
                .await
                .expect("sibling registered runtime"),
        );
        let target = sibling
            .initialize_project_graph_for_test(&target_root, open_options(&profile_root))
            .await
            .expect("sibling project graph");
        for graph in [&active, &target] {
            register_project(&active, &project_id_of(graph), graph.project_root()).await;
        }
        (tmp, active, target, sibling)
    }

    async fn denied_selector(cg: &TraceDecay, args: Value) -> TraceDecayError {
        let Err(error) = open_target_memory_db(cg, &args, Some(cg.profile_database())).await else {
            panic!("selector {args} must be denied instead of resolving a memory store");
        };
        error
    }

    async fn fact_count(target: &TargetMemoryDb<'_>) -> usize {
        memory_application(target)
            .unwrap()
            .memory_status_with_repair()
            .await
            .unwrap()
            .status
            .fact_count
    }

    async fn add_project_fact(cg: &TraceDecay, content: &str) {
        let owner = active_project_memory_owner(cg).unwrap();
        let memory = MemoryApplication::new(
            owner.clone(),
            cg.project_memory_db().await.unwrap().into_fact_store(),
        )
        .unwrap();
        assert!(
            memory
                .add_fact(
                    fact(content),
                    MemoryOperationContext::generated(&owner, content, None).unwrap(),
                )
                .await
                .unwrap()
                .fact
                .is_some(),
            "fixture fact '{content}' must persist"
        );
    }

    #[tokio::test]
    async fn active_project_memory_uses_the_served_database_handle() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path().join("project");
        let profile_root = tmp.path().join("profile");
        std::fs::create_dir_all(&project_root).unwrap();
        let cg = TraceDecay::init_with_options(&project_root, open_options(&profile_root))
            .await
            .unwrap();

        let target = open_target_memory_db(&cg, &json!({}), None).await.unwrap();

        assert!(matches!(target.db, ProjectMemoryDbHandle::Active(_)));
        assert!(std::ptr::eq(target.db(), cg.db()));
        assert_eq!(
            target.owner(),
            &project_memory_owner(cg.store_layout().identity.project_id.as_deref().unwrap())
                .unwrap()
        );
    }

    #[tokio::test]
    async fn project_selector_reads_the_selected_registered_projects_memory() {
        let (_tmp, active, target, _sibling_runtime) = cross_project_memory_pair().await;
        add_project_fact(&active, "active project selector fixture fact").await;
        for content in ["target selector fixture one", "target selector fixture two"] {
            add_project_fact(&target, content).await;
        }
        let target_project_id = project_id_of(&target);

        let selected = open_target_memory_db(
            &active,
            &json!({ "project_id": target_project_id }),
            Some(active.profile_database()),
        )
        .await
        .unwrap();

        assert_eq!(
            selected.owner(),
            &project_memory_owner(&target_project_id).unwrap()
        );
        assert_eq!(
            selected.project_root.canonicalize().unwrap(),
            target.project_root().canonicalize().unwrap()
        );
        assert_eq!(fact_count(&selected).await, 2);
    }

    #[tokio::test]
    async fn active_and_selected_project_memory_stay_isolated() {
        let (_tmp, active, target, _sibling_runtime) = cross_project_memory_pair().await;
        add_project_fact(&active, "active project selector fixture fact").await;
        add_project_fact(&target, "target project selector fixture fact").await;

        let active_target =
            open_target_memory_db(&active, &json!({}), Some(active.profile_database()))
                .await
                .unwrap();
        assert_eq!(fact_count(&active_target).await, 1);
        drop(active_target);

        let selected = open_target_memory_db(
            &active,
            &json!({ "project_id": project_id_of(&target) }),
            Some(active.profile_database()),
        )
        .await
        .unwrap();
        assert!(!std::ptr::eq(selected.db(), active.db()));
        assert_eq!(fact_count(&selected).await, 1);
    }

    #[tokio::test]
    async fn unresolved_project_selector_is_denied_without_falling_back() {
        let (_tmp, active, _target, _sibling_runtime) = cross_project_memory_pair().await;
        let error = denied_selector(&active, json!({ "project_id": "proj_does_not_exist" })).await;
        assert!(
            matches!(&error, TraceDecayError::Config { message }
                if message.contains("registered project not found for selector")),
            "{error}"
        );
    }

    #[tokio::test]
    async fn registered_project_without_profile_enrollment_is_denied() {
        let (tmp, active, _target, _sibling_runtime) = cross_project_memory_pair().await;
        let unenrolled_root = tmp.path().join("unenrolled");
        std::fs::create_dir_all(&unenrolled_root).unwrap();
        register_project(&active, "proj_unenrolled", &unenrolled_root).await;

        let error = denied_selector(&active, json!({ "project_id": "proj_unenrolled" })).await;
        assert!(
            matches!(&error, TraceDecayError::Config { message }
                if message.contains("is not enrolled in this TraceDecay profile")),
            "{error}"
        );
    }

    #[tokio::test]
    async fn ambiguous_project_name_selector_is_denied_as_ambiguous() {
        let (tmp, active, _target, _sibling_runtime) = cross_project_memory_pair().await;
        for (index, parent) in ["first", "second"].into_iter().enumerate() {
            let root = tmp.path().join(parent).join("shared");
            std::fs::create_dir_all(&root).unwrap();
            register_project(&active, &format!("proj_shared_{index}"), &root).await;
        }

        let error = denied_selector(&active, json!({ "project_path": "shared" })).await;
        assert!(
            matches!(&error, TraceDecayError::Config { message }
                if message.contains("is ambiguous across 2 registered projects")),
            "{error}"
        );
    }
}
