//! Cross-session and holographic memory handlers.

use std::path::{Path, PathBuf};

use serde_json::Value;
use tracedecay_domain::{FactOwnerV1, ProjectId};

use crate::application::memory::{
    MemoryApplication, MemoryApplicationError, MemoryOperationContext,
};
use crate::automation::memory_digest::refresh_memory_digest_after_memory_change;
use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::db::Database;
use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::memory::user::open_user_memory_db;
use crate::store::DatabaseFactStore;
use crate::store::memory::ProjectMemoryDbHandle;
use crate::tracedecay::TraceDecay;

use super::support::{
    profile_root_for_global_db, project_registry_context, project_selector_present,
};
use args::requests_user_memory;

mod actions;
mod args;
mod fact_store;
mod feedback;
mod registered_target;
mod status;

use registered_target::open_registered_project_memory_read_only;

pub(super) use fact_store::handle_fact_store;
pub(super) use feedback::handle_fact_feedback;
pub(super) use status::handle_memory_status;
pub(crate) use status::handle_user_memory_tool;

#[cfg(test)]
use serde_json::json;
#[cfg(test)]
use tracedecay_store::CompatibilityFeedbackRepairProgressV1;

#[cfg(test)]
use crate::memory::types::{AddFactRequest, MemoryCategory};

#[cfg(test)]
use args::MAX_FACT_LIMIT;
#[cfg(test)]
use fact_store::handle_fact_store_for_target;
#[cfg(test)]
use status::feedback_history_repair_payload;

pub(super) struct TargetMemoryDb<'a> {
    db: ProjectMemoryDbHandle<'a>,
    pub(super) project_root: PathBuf,
    pub(super) user_scope: bool,
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
        user_scope: true,
        owner: FactOwnerV1::Profile,
    })
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
            user_scope: false,
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
            user_scope: false,
            owner: project_memory_owner(selected_project_id)?,
        });
    }
    let owner = project_memory_owner(selected_project_id)?;
    let db = open_registered_project_memory_read_only(cg, &context).await?;
    Ok(TargetMemoryDb {
        db: ProjectMemoryDbHandle::Owned(Box::new(db)),
        project_root: PathBuf::from(&context.project.display_root),
        user_scope: false,
        owner,
    })
}

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

/// Upper bound on any single memory tool operation (add/search/feedback/…).
///
/// The add path performs a real per-fact holographic encode plus a serialized
/// write transaction and an optional digest refresh; measured add latency is
/// flat across fact count (no O(n) blow-up), but a contended write lock or a
/// starved host can still stretch one operation far past any interactive
/// budget. Without a bound the tool handler awaits the store indefinitely and
/// pins the MCP transport open. This deadline degrades such a stall to a typed,
/// retryable problem instead of an unbounded hang. It is deliberately generous
/// relative to normal sub-second latency so healthy operations never trip it.
const MEMORY_OPERATION_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// Typed "operation exceeded deadline" problem for a bounded memory operation.
///
/// Reuses the retryable [`TraceDecayError::ProjectRoute`] problem shape (a
/// stable `reason_code`, a `retryable` flag, and a human `detail`) so the MCP
/// boundary surfaces a structured, retryable error rather than a transport
/// hang. The deadline is a backstop, so retry is safe (writes are receipt
/// idempotent).
fn memory_deadline_error(operation: &str, deadline: std::time::Duration) -> TraceDecayError {
    TraceDecayError::project_route(
        "memory_operation_deadline_exceeded",
        true,
        format!(
            "memory {operation} operation exceeded the {}s deadline",
            deadline.as_secs()
        ),
    )
}

/// Bound `future` by [`MEMORY_OPERATION_DEADLINE`], mapping an elapsed deadline
/// to [`memory_deadline_error`]. See [`with_memory_deadline_for`].
pub(super) async fn with_memory_deadline<T>(
    operation: &str,
    future: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    with_memory_deadline_for(MEMORY_OPERATION_DEADLINE, operation, future).await
}

/// Bound `future` by `deadline`. On elapse the future is dropped (cancelling it
/// at its next suspension point) and a typed deadline error is returned; the
/// inner result is passed through unchanged otherwise.
pub(super) async fn with_memory_deadline_for<T>(
    deadline: std::time::Duration,
    operation: &str,
    future: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    match tokio::time::timeout(deadline, future).await {
        Ok(result) => result,
        Err(_elapsed) => Err(memory_deadline_error(operation, deadline)),
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

fn memory_operation_context(
    args: &Value,
    target_memory: &TargetMemoryDb<'_>,
    action: &str,
) -> Result<MemoryOperationContext> {
    if matches!(action, "search" | "probe" | "related" | "reason" | "list") {
        return match args.get("__mcp_request_id").and_then(Value::as_str) {
            Some(request_id) => MemoryOperationContext::from_trusted_request_id(
                target_memory.owner(),
                action,
                request_id,
                None,
            ),
            None => MemoryOperationContext::generated(target_memory.owner(), action, None),
        }
        .map_err(memory_application_error);
    }
    let mut logical_effect = args.clone();
    if let Some(effect) = logical_effect.as_object_mut() {
        effect.remove("__mcp_request_id");
    }
    MemoryOperationContext::from_logical_effect(
        target_memory.owner(),
        action,
        &logical_effect,
        None,
    )
    .map_err(memory_application_error)
}

async fn refresh_target_memory_digest(
    memory: &MemoryApplication<DatabaseFactStore<'_>>,
    target_memory: &TargetMemoryDb<'_>,
) {
    refresh_memory_digest_after_memory_change(memory, &target_memory.project_root).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_store::{
        CompatibilityFactSearchCursorV1, CompatibilityFactSearchFilterV1,
        CompatibilityFactSearchKindV1, CompatibilityFactSearchQuery, FactStoreError,
    };

    fn cursor_fact(content: &str) -> AddFactRequest {
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

    fn cursor_search_query(
        owner: FactOwnerV1,
        query: &str,
        after: Option<CompatibilityFactSearchCursorV1>,
    ) -> std::result::Result<CompatibilityFactSearchQuery, FactStoreError> {
        CompatibilityFactSearchQuery::with_filter(
            owner,
            CompatibilityFactSearchKindV1::Search,
            Some(query.to_owned()),
            CompatibilityFactSearchFilterV1::new(None, None, None)?,
            after,
            1,
        )
    }

    fn active_memory(cg: &TraceDecay) -> MemoryApplication<DatabaseFactStore<'_>> {
        MemoryApplication::new(
            active_project_memory_owner(cg).unwrap(),
            DatabaseFactStore::new(cg.db()),
        )
        .unwrap()
    }

    async fn empty_memory() -> (tempfile::TempDir, TraceDecay) {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path().join("project");
        let profile_root = tmp.path().join("profile");
        std::fs::create_dir_all(&project_root).unwrap();
        let cg = TraceDecay::init_with_options(
            &project_root,
            crate::tracedecay::TraceDecayOpenOptions {
                global_db_path: Some(profile_root.join("global.db")),
                profile_root: Some(profile_root),
            },
        )
        .await
        .unwrap();
        (tmp, cg)
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

    /// Two graphs enrolled in one profile, both registered in the profile
    /// registry the active graph reads selectors against.
    async fn cross_project_memory_pair() -> (tempfile::TempDir, TraceDecay, TraceDecay) {
        let tmp = tempfile::tempdir().unwrap();
        let profile_root = tmp.path().join("profile");
        let mut graphs = Vec::new();
        for name in ["active", "target"] {
            let project_root = tmp.path().join(name);
            std::fs::create_dir_all(&project_root).unwrap();
            graphs.push(
                TraceDecay::init_with_options(&project_root, open_options(&profile_root))
                    .await
                    .unwrap(),
            );
        }
        let target = graphs.pop().unwrap();
        let active = graphs.pop().unwrap();
        for graph in [&active, &target] {
            register_project(&active, &project_id_of(graph), graph.project_root()).await;
        }
        (tmp, active, target)
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
            .memory_status_with_repair_v1()
            .await
            .unwrap()
            .status
            .fact_count
    }

    async fn add_project_fact(cg: &TraceDecay, content: &str) {
        let owner = active_project_memory_owner(cg).unwrap();
        assert!(
            memory_application_for_project(cg)
                .await
                .add_fact_v1(
                    cursor_fact(content),
                    MemoryOperationContext::generated(&owner, content, None).unwrap(),
                )
                .await
                .unwrap()
                .fact
                .is_some(),
            "fixture fact '{content}' must persist"
        );
    }

    /// A memory application over the graph's *project-wide* store, so a
    /// branch-serving fixture seeds the same store a selector must resolve.
    async fn memory_application_for_project(
        cg: &TraceDecay,
    ) -> MemoryApplication<crate::store::memory::ProjectFactStore<'_>> {
        MemoryApplication::new(
            active_project_memory_owner(cg).unwrap(),
            cg.project_memory_db().await.unwrap().into_fact_store(),
        )
        .unwrap()
    }

    async fn seeded_memory() -> (tempfile::TempDir, TraceDecay, i64) {
        let (tmp, cg) = empty_memory().await;
        let owner = active_project_memory_owner(&cg).unwrap();
        let fact_id = active_memory(&cg)
            .add_fact_v1(
                AddFactRequest {
                    content: "existing fact".to_string(),
                    category: MemoryCategory::General,
                    source: None,
                    tags: Vec::new(),
                    entities: Vec::new(),
                    trust: None,
                    metadata: json!({}),
                },
                MemoryOperationContext::generated(&owner, "test-seed", None).unwrap(),
            )
            .await
            .unwrap()
            .fact
            .unwrap()
            .fact_id;
        (tmp, cg, fact_id)
    }

    #[tokio::test]
    async fn memory_deadline_passes_through_a_fast_result() {
        let ok: Result<u32> =
            with_memory_deadline_for(std::time::Duration::from_secs(30), "fast probe", async {
                Ok(7)
            })
            .await;
        assert_eq!(ok.unwrap(), 7);

        let err = with_memory_deadline_for(std::time::Duration::from_secs(30), "fast err", async {
            Err::<u32, _>(config_error("inner failure"))
        })
        .await
        .unwrap_err();
        assert!(
            matches!(err, TraceDecayError::Config { message } if message == "inner failure"),
            "the wrapper must pass an inner error through unchanged"
        );
    }

    #[tokio::test]
    async fn memory_deadline_elapse_yields_a_typed_retryable_problem() {
        let err = with_memory_deadline_for(
            std::time::Duration::from_millis(10),
            "fact_store add",
            async {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                Ok::<u32, TraceDecayError>(0)
            },
        )
        .await
        .unwrap_err();
        let (reason_code, retryable, detail) = err
            .project_route_context()
            .expect("an elapsed memory deadline must surface a typed project-route problem");
        assert_eq!(reason_code, "memory_operation_deadline_exceeded");
        assert!(retryable, "a deadline backstop is safe to retry");
        assert!(
            detail.contains("fact_store add") && detail.contains("deadline"),
            "detail must name the operation and the deadline: {detail}"
        );
    }

    /// The add path (holographic encode + serialized write) must finish well
    /// inside the operation deadline in a clean tempdir. This is the direct
    /// regression for the reported unbounded hang: an add completes promptly
    /// rather than running until the transport is abandoned.
    #[tokio::test]
    async fn add_fact_completes_within_the_operation_deadline() {
        let (_tmp, cg) = empty_memory().await;
        let owner = active_project_memory_owner(&cg).unwrap();
        let memory = active_memory(&cg);
        let outcome =
            with_memory_deadline_for(MEMORY_OPERATION_DEADLINE, "fact_store add", async {
                memory
                    .add_fact_v1(
                        AddFactRequest {
                            content: "deadline-bounded add fixture".to_string(),
                            category: MemoryCategory::General,
                            source: None,
                            tags: Vec::new(),
                            entities: vec!["fixture-entity".to_string()],
                            trust: None,
                            metadata: json!({}),
                        },
                        MemoryOperationContext::generated(&owner, "deadline-bounded add", None)
                            .unwrap(),
                    )
                    .await
                    .map_err(memory_application_error)
            })
            .await
            .expect("a clean add must complete within the operation deadline");
        assert!(
            outcome.fact.is_some(),
            "the bounded add must persist a fact"
        );
    }

    /// Manual scaling probe (ignored in CI to avoid timing flakiness). Recorded
    /// evidence: per-add latency is flat from add #1 through #600 (~50ms each in
    /// a debug build), confirming the add path is not O(n) over existing facts.
    /// Run with `--ignored --nocapture` to reproduce the per-milestone timings.
    #[ignore = "manual timing probe: documents flat (non-O(n)) add scaling"]
    #[tokio::test]
    async fn add_fact_latency_is_flat_across_fact_count_probe() {
        let (_tmp, cg) = empty_memory().await;
        let owner = active_project_memory_owner(&cg).unwrap();
        let memory = active_memory(&cg);
        let mut milestones = Vec::new();
        for i in 0..600usize {
            let content = format!("timing probe fact number {i} with some distinct payload text");
            let start = std::time::Instant::now();
            memory
                .add_fact_v1(
                    AddFactRequest {
                        content: content.clone(),
                        category: MemoryCategory::General,
                        source: None,
                        tags: Vec::new(),
                        entities: vec![format!("entity-{}", i % 7)],
                        trust: None,
                        metadata: json!({}),
                    },
                    MemoryOperationContext::generated(&owner, &content, None).unwrap(),
                )
                .await
                .unwrap();
            let elapsed = start.elapsed();
            if matches!(i, 0 | 9 | 49 | 99 | 199 | 299 | 399 | 499 | 599) {
                milestones.push((i + 1, elapsed));
                eprintln!("ADD #{:>4} took {:?}", i + 1, elapsed);
            }
        }
        eprintln!("TIMING_PROBE_MILESTONES {milestones:?}");
    }

    #[tokio::test]
    async fn active_project_memory_uses_the_served_database_handle() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path().join("project");
        let profile_root = tmp.path().join("profile");
        std::fs::create_dir_all(&project_root).unwrap();
        let cg = TraceDecay::init_with_options(
            &project_root,
            crate::tracedecay::TraceDecayOpenOptions {
                global_db_path: Some(profile_root.join("global.db")),
                profile_root: Some(profile_root),
            },
        )
        .await
        .unwrap();

        let target = open_target_memory_db(&cg, &json!({}), None).await.unwrap();

        assert!(matches!(target.db, ProjectMemoryDbHandle::Active(_)));
        assert!(std::ptr::eq(target.db(), cg.db()));
        assert_eq!(
            target.owner(),
            &project_memory_owner(cg.store_layout().identity.project_id.as_deref().unwrap(),)
                .unwrap()
        );
    }

    #[tokio::test]
    async fn project_selector_reads_the_selected_registered_projects_memory() {
        let (_tmp, active, target) = cross_project_memory_pair().await;
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
        let (_tmp, active, target) = cross_project_memory_pair().await;
        add_project_fact(&active, "active project selector fixture fact").await;
        add_project_fact(&target, "target selector fixture fact").await;

        let default_scope =
            open_target_memory_db(&active, &json!({}), Some(active.profile_database()))
                .await
                .unwrap();
        assert_eq!(fact_count(&default_scope).await, 1);
        assert_eq!(
            default_scope.owner(),
            &project_memory_owner(&project_id_of(&active)).unwrap()
        );
        drop(default_scope);

        let selected = open_target_memory_db(
            &active,
            &json!({ "project_id": project_id_of(&target) }),
            Some(active.profile_database()),
        )
        .await
        .unwrap();
        assert!(
            !std::ptr::eq(selected.db(), active.db()),
            "a foreign project selector must not resolve the active project's database"
        );
        assert_eq!(fact_count(&selected).await, 1);

        // Reading the selected project must leave the active project's own
        // memory untouched, not merge the two owners' facts.
        assert_eq!(
            memory_application_for_project(&active)
                .await
                .list_facts_untracked_v1(None, None, 10)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn unresolved_project_selector_is_denied_without_falling_back() {
        let (_tmp, active, _target) = cross_project_memory_pair().await;

        let error = denied_selector(&active, json!({ "project_id": "proj_does_not_exist" })).await;

        assert!(
            matches!(&error, TraceDecayError::Config { message }
                if message.contains("registered project not found for selector")),
            "{error}"
        );
    }

    #[tokio::test]
    async fn registered_project_without_profile_enrollment_is_denied() {
        let (tmp, active, _target) = cross_project_memory_pair().await;
        // Registered in the profile registry, but never opened here, so no
        // enrollment marker names it and this profile holds no memory store.
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
        let (tmp, active, _target) = cross_project_memory_pair().await;
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

    #[tokio::test]
    async fn branch_serving_selector_resolves_the_project_wide_memory_store() {
        fn git(project: &Path, args: &[&str]) {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(project)
                .env("GIT_AUTHOR_NAME", "TraceDecay Test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "TraceDecay Test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} must succeed");
        }

        let tmp = tempfile::tempdir().unwrap();
        let profile_root = tmp.path().join("profile");
        let project_root = tmp.path().join("project");
        std::fs::create_dir_all(project_root.join("src")).unwrap();
        std::fs::write(project_root.join("src/lib.rs"), "pub fn served() {}\n").unwrap();
        git(&project_root, &["init", "-b", "main"]);
        git(&project_root, &["add", "."]);
        git(&project_root, &["commit", "-m", "initial"]);

        let main = TraceDecay::init_with_options(&project_root, open_options(&profile_root))
            .await
            .unwrap();
        main.index_all().await.unwrap();
        let project_id = project_id_of(&main);
        let project_db_path = main.store_layout().graph_db_path.clone();
        main.checkpoint().await.unwrap();
        main.close();

        git(&project_root, &["checkout", "-b", "feature"]);
        TraceDecay::add_branch_tracking_with_options(
            &project_root,
            "feature",
            open_options(&profile_root),
        )
        .await
        .unwrap();

        let branch = TraceDecay::open_with_options(&project_root, open_options(&profile_root))
            .await
            .unwrap();
        assert_eq!(branch.serving_branch(), Some("feature"));
        assert_ne!(
            branch.db_path(),
            project_db_path,
            "fixture must serve a branch shard distinct from the project store"
        );
        register_project(&branch, &project_id, &project_root).await;
        add_project_fact(&branch, "durable facts stay project-wide across branches").await;

        let selected = open_target_memory_db(
            &branch,
            &json!({ "project_id": project_id }),
            Some(branch.profile_database()),
        )
        .await
        .unwrap();

        assert!(
            !std::ptr::eq(selected.db(), branch.db()),
            "a branch-serving graph must resolve memory to the project store, not its shard"
        );
        assert_eq!(fact_count(&selected).await, 1);
        drop(selected);
        branch.checkpoint().await.unwrap();
        branch.close();
    }

    #[tokio::test]
    async fn feedback_rejects_cross_project_write_before_opening_a_store() {
        let (_tmp, cg, fact_id) = seeded_memory().await;

        let error = handle_fact_feedback(
            &cg,
            json!({
                "fact_id": fact_id,
                "action": "helpful",
                "project_id": "another_project",
            }),
            None,
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            TraceDecayError::Config { ref message }
                if message.contains("cross-project fact_feedback writes")
        ));
    }

    #[tokio::test]
    async fn fact_feedback_without_source_keeps_legacy_mcp_history() {
        let (_tmp, cg, fact_id) = seeded_memory().await;

        handle_fact_feedback(
            &cg,
            json!({ "fact_id": fact_id, "action": "helpful" }),
            None,
        )
        .await
        .unwrap();

        let history = active_memory(&cg)
            .fact_trust_history_v1(fact_id, MAX_FACT_LIMIT)
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].source, "mcp");
    }

    #[tokio::test]
    async fn trusted_fact_feedback_id_replays_without_duplicate_history() {
        let (_tmp, cg, fact_id) = seeded_memory().await;
        let args = json!({
            "fact_id": fact_id,
            "action": "helpful",
            "__mcp_request_id": "same-feedback-json-rpc-request",
        });

        for _ in 0..2 {
            handle_fact_feedback(&cg, args.clone(), None).await.unwrap();
        }

        let history = active_memory(&cg)
            .fact_trust_history_v1(fact_id, MAX_FACT_LIMIT)
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
    }

    #[tokio::test]
    async fn fact_add_replays_after_reconnection_changes_request_correlation() {
        let (_tmp, cg) = empty_memory().await;
        let first = handle_fact_store(
            &cg,
            json!({
                "action": "add",
                "content": "stable logical memory write",
                "__mcp_request_id": "request.mcp.connection-a.first",
            }),
            None,
        )
        .await
        .unwrap();
        let replay = handle_fact_store(
            &cg,
            json!({
                "action": "add",
                "content": "stable logical memory write",
                "__mcp_request_id": "request.mcp.connection-b.first",
            }),
            None,
        )
        .await
        .unwrap();

        assert_eq!(replay.value, first.value);
        assert_eq!(
            active_memory(&cg)
                .list_facts_untracked_v1(None, None, 10)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn fact_add_accepts_request_derived_operation_as_legacy_replay() {
        let (_tmp, cg) = empty_memory().await;
        let owner = active_project_memory_owner(&cg).unwrap();
        active_memory(&cg)
            .add_fact_v1(
                AddFactRequest {
                    content: "legacy request-derived memory write".to_owned(),
                    category: MemoryCategory::General,
                    source: None,
                    tags: Vec::new(),
                    entities: Vec::new(),
                    trust: None,
                    metadata: json!({}),
                },
                MemoryOperationContext::from_trusted_request_id(
                    &owner,
                    "add",
                    "request.mcp.legacy-connection.first",
                    None,
                )
                .unwrap(),
            )
            .await
            .unwrap();

        let replay = handle_fact_store(
            &cg,
            json!({
                "action": "add",
                "content": "legacy request-derived memory write",
                "__mcp_request_id": "request.mcp.reconnected.first",
            }),
            None,
        )
        .await
        .unwrap();
        let rendered = replay.value.to_string();

        assert!(rendered.contains("**diff:** add"), "{rendered}");
        assert!(!rendered.contains("near_duplicate"), "{rendered}");
        assert_eq!(
            active_memory(&cg)
                .list_facts_untracked_v1(None, None, 10)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn incomplete_feedback_history_repair_is_explicit() {
        assert_eq!(
            feedback_history_repair_payload(CompatibilityFeedbackRepairProgressV1::Incomplete {
                processed: 1,
                remaining: Some(2),
            }),
            json!({
                "state": "incomplete",
                "processed": 1,
                "remaining": 2,
            })
        );
    }

    #[tokio::test]
    async fn pure_fact_reads_do_not_wait_for_the_writer_lane() {
        let (_tmp, cg, fact_id) = seeded_memory().await;
        let writer = cg
            .db()
            .writer_connection("hold memory tool writer")
            .await
            .unwrap();
        let target = TargetMemoryDb {
            db: ProjectMemoryDbHandle::Active(cg.db()),
            project_root: cg.project_root().to_path_buf(),
            user_scope: true,
            owner: active_project_memory_owner(&cg).unwrap(),
        };

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            handle_fact_store_for_target(
                json!({ "action": "get", "fact_id": fact_id }),
                false,
                target,
            ),
        )
        .await
        .expect("pure reads must not wait for writer authority")
        .unwrap();
        let rendered = result.value.to_string();
        assert!(rendered.contains("existing fact"), "{rendered}");
        drop(writer);
    }

    #[tokio::test]
    async fn local_fact_search_records_retrieval_without_snapshot_deadlock() {
        let (_tmp, cg, fact_id) = seeded_memory().await;
        let target = TargetMemoryDb {
            db: ProjectMemoryDbHandle::Active(cg.db()),
            project_root: cg.project_root().to_path_buf(),
            user_scope: true,
            owner: active_project_memory_owner(&cg).unwrap(),
        };

        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            handle_fact_store_for_target(
                json!({ "action": "search", "query": "existing fact" }),
                false,
                target,
            ),
        )
        .await
        .expect("local retrieval-counting actions must not hold a read snapshot")
        .unwrap();

        let fact = active_memory(&cg)
            .get_fact_v1(fact_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fact.retrieval_count, 1);
        assert_eq!(fact.access_count, 1);
    }

    #[tokio::test]
    async fn fact_mutations_wait_for_the_writer_lane_before_starting_a_transaction() {
        let (_tmp, cg, _) = seeded_memory().await;
        let writer = cg
            .db()
            .writer_connection("hold memory mutation writer")
            .await
            .unwrap();
        let target = TargetMemoryDb {
            db: ProjectMemoryDbHandle::Active(cg.db()),
            project_root: cg.project_root().to_path_buf(),
            user_scope: true,
            owner: active_project_memory_owner(&cg).unwrap(),
        };
        let mut add = Box::pin(handle_fact_store_for_target(
            json!({ "action": "add", "content": "concurrent fact" }),
            false,
            target,
        ));

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(25), &mut add)
                .await
                .is_err()
        );
        drop(writer);
        add.await.unwrap();
        assert_eq!(
            active_memory(&cg)
                .list_facts_untracked_v1(None, None, 10)
                .await
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn retrieval_counter_writes_wait_for_the_writer_lane() {
        let (_tmp, cg, fact_id) = seeded_memory().await;
        let writer = cg
            .db()
            .writer_connection("hold memory retrieval writer")
            .await
            .unwrap();
        let target = TargetMemoryDb {
            db: ProjectMemoryDbHandle::Active(cg.db()),
            project_root: cg.project_root().to_path_buf(),
            user_scope: true,
            owner: active_project_memory_owner(&cg).unwrap(),
        };
        let mut record = Box::pin(handle_fact_store_for_target(
            json!({ "action": "search", "query": "existing fact" }),
            false,
            target,
        ));

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(25), &mut record)
                .await
                .is_err()
        );
        drop(writer);
        record.await.unwrap();
        assert_eq!(
            active_memory(&cg)
                .get_fact_v1(fact_id)
                .await
                .unwrap()
                .unwrap()
                .retrieval_count,
            1
        );
        assert_eq!(
            active_memory(&cg)
                .get_fact_v1(fact_id)
                .await
                .unwrap()
                .unwrap()
                .access_count,
            1
        );
    }

    #[tokio::test]
    async fn trusted_memory_retrieval_id_replays_without_double_counting() {
        let (_tmp, cg, fact_id) = seeded_memory().await;
        for _ in 0..2 {
            let target = TargetMemoryDb {
                db: ProjectMemoryDbHandle::Active(cg.db()),
                project_root: cg.project_root().to_path_buf(),
                user_scope: true,
                owner: active_project_memory_owner(&cg).unwrap(),
            };
            handle_fact_store_for_target(
                json!({
                    "action": "search",
                    "query": "existing fact",
                    "__mcp_request_id": "same-json-rpc-request",
                }),
                false,
                target,
            )
            .await
            .unwrap();
        }
        let fact = active_memory(&cg)
            .get_fact_v1(fact_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fact.retrieval_count, 1);
        assert_eq!(fact.access_count, 1);
    }

    #[tokio::test]
    async fn cross_project_memory_retrieval_is_untracked() {
        let (_tmp, cg, fact_id) = seeded_memory().await;
        let target = TargetMemoryDb {
            db: ProjectMemoryDbHandle::Active(cg.db()),
            project_root: cg.project_root().to_path_buf(),
            user_scope: true,
            owner: active_project_memory_owner(&cg).unwrap(),
        };
        handle_fact_store_for_target(
            json!({ "action": "search", "query": "existing fact" }),
            true,
            target,
        )
        .await
        .unwrap();
        let fact = active_memory(&cg)
            .get_fact_v1(fact_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fact.retrieval_count, 0);
        assert_eq!(fact.access_count, 0);
    }

    #[tokio::test]
    async fn compatibility_search_cursor_replays_and_rejects_other_owners() {
        let (_tmp, cg) = empty_memory().await;
        let owner = active_project_memory_owner(&cg).unwrap();
        let memory = active_memory(&cg);
        for (operation, content) in [
            (
                "test-project-cursor-one",
                "cursor fixture marigold topology",
            ),
            ("test-project-cursor-two", "cursor fixture basalt workflow"),
        ] {
            assert!(
                memory
                    .add_fact_v1(
                        cursor_fact(content),
                        MemoryOperationContext::generated(&owner, operation, None).unwrap(),
                    )
                    .await
                    .unwrap()
                    .fact
                    .is_some(),
                "{operation} must persist a real fixture fact"
            );
        }

        let first_page = memory
            .search_compatibility_facts(
                cursor_search_query(owner.clone(), "cursor fixture", None).unwrap(),
            )
            .await
            .unwrap();
        let cursor = first_page
            .next_after()
            .cloned()
            .expect("the first finite page must provide its real cursor");
        let second_page = memory
            .search_compatibility_facts(
                cursor_search_query(owner.clone(), "cursor fixture", Some(cursor.clone())).unwrap(),
            )
            .await
            .unwrap();
        let replay_page = memory
            .search_compatibility_facts(
                cursor_search_query(owner.clone(), "cursor fixture", Some(cursor)).unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(first_page.owner(), &owner);
        assert_eq!(first_page.hits().len(), 1);
        assert_eq!(second_page.hits().len(), 1);
        assert!(
            second_page.next_after().is_none(),
            "the two real fixture facts must exhaust the second page"
        );
        assert_ne!(
            first_page.hits()[0].fact().fact_id(),
            second_page.hits()[0].fact().fact_id()
        );
        let first = &first_page.hits()[0];
        let second = &second_page.hits()[0];
        assert!(
            first.score_millionths() > second.score_millionths()
                || (first.score_millionths() == second.score_millionths()
                    && (first.fact().telemetry().updated_at()
                        > second.fact().telemetry().updated_at()
                        || (first.fact().telemetry().updated_at()
                            == second.fact().telemetry().updated_at()
                            && first.fact().fact_id() < second.fact().fact_id()))),
            "search pages must preserve canonical score, timestamp, and fact-id ordering"
        );
        assert_eq!(replay_page, second_page);

        let profile_owner = FactOwnerV1::Profile;
        let profile_memory =
            MemoryApplication::new(profile_owner.clone(), DatabaseFactStore::new(cg.db())).unwrap();
        for (operation, content) in [
            (
                "test-profile-cursor-one",
                "profile cursor fixture violet semantics",
            ),
            (
                "test-profile-cursor-two",
                "profile cursor fixture amber provenance",
            ),
        ] {
            assert!(
                profile_memory
                    .add_fact_v1(
                        cursor_fact(content),
                        MemoryOperationContext::generated(&profile_owner, operation, None).unwrap(),
                    )
                    .await
                    .unwrap()
                    .fact
                    .is_some(),
                "{operation} must persist a real fixture fact"
            );
        }
        let profile_first_page = profile_memory
            .search_compatibility_facts(
                cursor_search_query(profile_owner, "profile cursor fixture", None).unwrap(),
            )
            .await
            .unwrap();
        let foreign_cursor = profile_first_page
            .next_after()
            .cloned()
            .expect("the profile page must provide its real cursor");
        assert!(matches!(
            cursor_search_query(owner, "profile cursor fixture", Some(foreign_cursor)),
            Err(FactStoreError::OwnerMismatch)
        ));
    }
}
