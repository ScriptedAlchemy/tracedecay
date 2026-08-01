//! Downward graph-runtime port used by transport-neutral use cases.

use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use tracedecay_application::retrieval::HealthDeltaResult;
use tracedecay_application::retrieval::grep_analysis::{RedundancyRequestV1, RedundancyResultV1};
use tracedecay_application::source_edit::{
    AstGrepResult, EditResult, InsertResult, MoveResult, MultiEditResult,
};
use tracedecay_application::{ApiMigrationApplyResultV1, ApiMigrationPlanV1};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::errors::Result;
use tracedecay_runtime_core::storage::StoreLayout;
use tracedecay_runtime_core::types::{Edge, GraphStats, Node, NodeKind, SearchResult, Subgraph};

pub type GraphFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;
pub type GraphValueFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, Serialize)]
pub struct TrackedBranchDiagnostic {
    pub name: String,
    pub db_file: String,
    pub db_path: PathBuf,
    pub db_exists: bool,
    pub size_bytes: u64,
    pub parent: Option<String>,
    pub parent_db_path: Option<PathBuf>,
    pub parent_db_exists: Option<bool>,
    pub created_at: String,
    pub last_synced_at: String,
    pub is_default: bool,
    pub is_current: bool,
    pub is_open_active: bool,
    pub is_serving: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BranchDiagnostics {
    pub tracking_enabled: bool,
    pub default_branch: Option<String>,
    pub current_branch: Option<String>,
    pub open_active_branch: Option<String>,
    pub serving_branch: Option<String>,
    pub serving_db_path: PathBuf,
    pub serving_db_exists: bool,
    pub branch_drifted: bool,
    pub branch_resolution: String,
    pub is_fallback: bool,
    pub fallback_target: Option<String>,
    pub fallback_warning: Option<String>,
    pub live_branch_tracked: bool,
    pub live_branch_db_path: Option<PathBuf>,
    pub live_branch_db_exists: Option<bool>,
    pub nearest_tracked_ancestor: Option<String>,
    pub nearest_tracked_ancestor_db_path: Option<PathBuf>,
    pub nearest_tracked_ancestor_db_exists: Option<bool>,
    pub tracked_branch_count: usize,
    pub branches: Vec<TrackedBranchDiagnostic>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct EditDiagnosticRecord {
    pub file: String,
    pub line_start: u32,
    pub level: String,
    pub code: Option<String>,
    pub message: String,
}

pub trait GraphRuntimePort: Send + Sync {
    fn project_root(&self) -> &Path;
    fn db(&self) -> &Database;
    fn db_path(&self) -> PathBuf;
    fn store_layout(&self) -> &StoreLayout;
    fn is_read_only(&self) -> bool;
    fn branch_diagnostics(&self) -> BranchDiagnostics;

    fn get_node<'a>(&'a self, id: &'a str) -> GraphFuture<'a, Option<Node>>;
    fn get_nodes_by_file<'a>(&'a self, file: &'a str) -> GraphFuture<'a, Vec<Node>>;
    fn get_nodes_by_name<'a>(&'a self, name: &'a str) -> GraphFuture<'a, Vec<Node>>;
    fn get_nodes_by_qualified_name<'a>(
        &'a self,
        qualified_name: &'a str,
    ) -> GraphFuture<'a, Vec<Node>>;
    fn search<'a>(&'a self, query: &'a str, limit: usize) -> GraphFuture<'a, Vec<SearchResult>>;
    fn get_stats(&self) -> GraphFuture<'_, GraphStats>;
    fn get_all_nodes(&self) -> GraphFuture<'_, Vec<Node>>;
    fn get_all_edges(&self) -> GraphFuture<'_, Vec<Edge>>;
    fn get_incoming_edges<'a>(&'a self, node_id: &'a str) -> GraphFuture<'a, Vec<Edge>>;
    fn get_outgoing_edges<'a>(&'a self, node_id: &'a str) -> GraphFuture<'a, Vec<Edge>>;
    fn get_callers<'a>(
        &'a self,
        node_id: &'a str,
        max_depth: usize,
    ) -> GraphFuture<'a, Vec<(Node, Edge)>>;
    fn get_callees<'a>(
        &'a self,
        node_id: &'a str,
        max_depth: usize,
    ) -> GraphFuture<'a, Vec<(Node, Edge)>>;
    fn get_call_chain<'a>(
        &'a self,
        from_id: &'a str,
        to_id: &'a str,
        max_depth: usize,
    ) -> GraphFuture<'a, Option<Vec<(Node, Option<Edge>)>>>;
    fn get_impact_radius<'a>(
        &'a self,
        node_id: &'a str,
        max_depth: usize,
    ) -> GraphFuture<'a, Subgraph>;
    fn get_impact_radius_multi<'a>(
        &'a self,
        seed_ids: &'a [String],
        max_depth: usize,
    ) -> GraphFuture<'a, Vec<Node>>;
    fn get_trait_dispatch_targets<'a>(&'a self, method: &'a Node) -> GraphFuture<'a, Vec<Node>>;
    fn get_test_annotated_node_ids<'a>(
        &'a self,
        candidate_ids: &'a [String],
    ) -> GraphFuture<'a, std::collections::HashSet<String>>;
    fn get_files_with_test_annotations(&self)
    -> GraphFuture<'_, std::collections::HashSet<String>>;
    fn get_file_dependents<'a>(&'a self, file: &'a str) -> GraphFuture<'a, Vec<String>>;
    fn node_at_location<'a>(
        &'a self,
        file: &'a str,
        line_1based: u32,
    ) -> GraphFuture<'a, Option<Node>>;
    fn last_synced_commit(&self) -> GraphValueFuture<'_, Option<String>>;
    fn storage_page_counts(&self) -> GraphFuture<'_, (u64, u64, u64)>;
    fn get_complexity_ranked<'a>(
        &'a self,
        node_kind: Option<&'a NodeKind>,
        path_prefix: Option<&'a str>,
        limit: usize,
    ) -> GraphFuture<'a, Vec<(Node, u32, u64, u64, u64)>>;
    fn run_diagnostics<'a>(&'a self, file: &'a str) -> GraphFuture<'a, Vec<EditDiagnosticRecord>>;
    fn redundancy<'a>(
        &'a self,
        request: &'a RedundancyRequestV1,
        scope_prefix: Option<&'a str>,
    ) -> GraphFuture<'a, RedundancyResultV1>;
    fn health_delta<'a>(
        &'a self,
        observation_database: &'a RegisteredGlobalDb,
        before_cursor: Option<&'a str>,
        path_prefix: Option<&'a str>,
    ) -> GraphFuture<'a, HealthDeltaResult>;
    fn replace_symbol<'a>(
        &'a self,
        symbol: &'a str,
        new_source: &'a str,
        dry_run: bool,
    ) -> GraphFuture<'a, EditResult>;
    fn str_replace<'a>(
        &'a self,
        path: &'a str,
        old_str: &'a str,
        new_str: &'a str,
        dry_run: bool,
    ) -> GraphFuture<'a, EditResult>;
    fn multi_str_replace<'a>(
        &'a self,
        path: &'a str,
        replacements: &'a [(&'a str, &'a str)],
        dry_run: bool,
    ) -> GraphFuture<'a, MultiEditResult>;
    fn insert_at<'a>(
        &'a self,
        path: &'a str,
        anchor: &'a str,
        content: &'a str,
        before: bool,
        dry_run: bool,
    ) -> GraphFuture<'a, InsertResult>;
    fn insert_at_symbol<'a>(
        &'a self,
        symbol: &'a str,
        content: &'a str,
        position: &'a str,
        dry_run: bool,
    ) -> GraphFuture<'a, InsertResult>;
    fn ast_grep_rewrite<'a>(
        &'a self,
        path: &'a str,
        pattern: &'a str,
        rewrite: &'a str,
        dry_run: bool,
    ) -> GraphFuture<'a, AstGrepResult>;
    fn move_symbol<'a>(
        &'a self,
        symbol: &'a str,
        dest_file: &'a str,
        dry_run: bool,
        update_references: bool,
    ) -> GraphFuture<'a, MoveResult>;
    fn apply_api_migration_plan<'a>(
        &'a self,
        plan: &'a ApiMigrationPlanV1,
        dry_run: bool,
        is_cancelled: &'a mut (dyn FnMut() -> bool + Send),
    ) -> GraphFuture<'a, ApiMigrationApplyResultV1>;
    fn rollback_api_migration_plan<'a>(
        &'a self,
        plan: &'a ApiMigrationPlanV1,
    ) -> GraphFuture<'a, ()>;
    fn recover_source_edit_preimages<'a>(
        &'a self,
        files: &'a [PlannedSourceEditFile],
    ) -> GraphFuture<'a, ()>;
}

pub type TraceDecay = dyn GraphRuntimePort;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedSourceEditFile {
    pub relative_path: String,
    pub expected: Option<String>,
    pub intended: Option<String>,
}

#[derive(Debug)]
struct SourceEditApplyState {
    files: Vec<PlannedSourceEditFile>,
    consumed: BTreeSet<String>,
}

tokio::task_local! {
    static SOURCE_EDIT_PLAN_CAPTURE: Arc<Mutex<Vec<PlannedSourceEditFile>>>;
    static SOURCE_EDIT_APPLY_STATE: Arc<Mutex<SourceEditApplyState>>;
}

pub async fn capture_source_edit_plan<T>(
    future: impl Future<Output = T>,
) -> (T, Vec<PlannedSourceEditFile>) {
    let capture = Arc::new(Mutex::new(Vec::new()));
    let result = SOURCE_EDIT_PLAN_CAPTURE
        .scope(Arc::clone(&capture), future)
        .await;
    let files = capture
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    (result, files)
}

pub async fn apply_source_edit_plan<T>(
    files: Vec<PlannedSourceEditFile>,
    future: impl Future<Output = T>,
) -> (T, bool) {
    let expected_count = files.len();
    let state = Arc::new(Mutex::new(SourceEditApplyState {
        files,
        consumed: BTreeSet::new(),
    }));
    let result = SOURCE_EDIT_APPLY_STATE
        .scope(Arc::clone(&state), future)
        .await;
    let complete = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .consumed
        .len()
        == expected_count;
    (result, complete)
}

pub fn capture_planned_source_edit(
    relative_path: &str,
    expected: Option<&str>,
    intended: Option<&str>,
) {
    let _ = SOURCE_EDIT_PLAN_CAPTURE.try_with(|capture| {
        capture
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(PlannedSourceEditFile {
                relative_path: relative_path.to_owned(),
                expected: expected.map(str::to_owned),
                intended: intended.map(str::to_owned),
            });
    });
}

pub fn validate_planned_source_edit(
    relative_path: &str,
    expected: Option<&str>,
    intended: Option<&str>,
) -> Result<()> {
    SOURCE_EDIT_APPLY_STATE
        .try_with(|state| {
            let mut state = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(planned) = state
                .files
                .iter()
                .find(|file| file.relative_path == relative_path)
            else {
                return Err(tracedecay_runtime_core::errors::TraceDecayError::Config {
                    message: format!(
                        "source edit apply produced unplanned candidate {relative_path}"
                    ),
                });
            };
            if planned.expected.as_deref() != expected || planned.intended.as_deref() != intended {
                return Err(tracedecay_runtime_core::errors::TraceDecayError::Config {
                    message: format!(
                        "source edit candidate {relative_path} drifted from its exact preview"
                    ),
                });
            }
            state.consumed.insert(relative_path.to_owned());
            Ok(())
        })
        .unwrap_or(Ok(()))
}

pub fn validate_source_edit_candidate_parent(project_root: &Path, relative: &Path) -> Result<()> {
    let _ = source_edit_candidate_path(project_root, relative)?;
    Ok(())
}

pub fn read_source_edit_candidate(project_root: &Path, relative: &Path) -> Result<Option<Vec<u8>>> {
    let path = source_edit_candidate_path(project_root, relative)?;
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn source_edit_candidate_path(project_root: &Path, relative: &Path) -> Result<PathBuf> {
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(tracedecay_runtime_core::errors::TraceDecayError::Config {
            message: "source edit path is not a regular file beneath the authorized worktree"
                .to_owned(),
        });
    }
    let canonical_root = project_root.canonicalize()?;
    let path = canonical_root.join(relative);
    let parent =
        path.parent().ok_or_else(
            || tracedecay_runtime_core::errors::TraceDecayError::Config {
                message: "source edit path has no parent".to_owned(),
            },
        )?;
    let canonical_parent = parent.canonicalize()?;
    if !canonical_parent.starts_with(&canonical_root) {
        return Err(tracedecay_runtime_core::errors::TraceDecayError::Config {
            message: "source edit path escapes the authorized worktree".to_owned(),
        });
    }
    Ok(canonical_parent.join(path.file_name().ok_or_else(|| {
        tracedecay_runtime_core::errors::TraceDecayError::Config {
            message: "source edit path has no file name".to_owned(),
        }
    })?))
}

pub struct SyncLockGuard(File);

impl Drop for SyncLockGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

pub fn try_acquire_sync_lock_at(lock_path: &Path) -> Result<SyncLockGuard> {
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    file.try_lock_exclusive().map_err(|error| {
        tracedecay_runtime_core::errors::TraceDecayError::SyncLock {
            message: format!("could not lock sync lockfile: {error}"),
        }
    })?;
    Ok(SyncLockGuard(file))
}
