//! Narrow root-owned source authorities used by transport-neutral use cases.

mod runtime_port;

pub use runtime_port::{ProjectStoreRuntimeV1, RuntimeFuture};

use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use tracedecay_application::source_edit::{
    AstGrepResult, EditResult, InsertResult, MoveResult, MultiEditResult, RenameResult,
    RenameSymbolBindingV1,
};
use tracedecay_code_index::graph_projection::CodeGraphInteractiveReader;
use tracedecay_domain::errors::Result;
use tracedecay_graph_db::GraphCancellation;
use tracedecay_runtime_core::storage::StoreLayout;

pub type GraphFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// One application-admitted immutable graph generation used for a source-edit
/// plan and its exact preview/apply identity.
#[derive(Clone)]
pub struct SourceEditGraphReadV1 {
    reader: CodeGraphInteractiveReader,
    cancellation: Arc<dyn GraphCancellation>,
}

impl SourceEditGraphReadV1 {
    pub fn new(
        reader: CodeGraphInteractiveReader,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Self {
        Self {
            reader,
            cancellation,
        }
    }

    pub fn reader(&self) -> &CodeGraphInteractiveReader {
        &self.reader
    }

    pub fn cancellation(&self) -> Arc<dyn GraphCancellation> {
        Arc::clone(&self.cancellation)
    }
}

#[derive(Debug, Clone)]
pub struct EditDiagnosticRecord {
    pub file: String,
    pub line_start: u32,
    pub level: String,
    pub code: Option<String>,
    pub message: String,
}

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

/// Narrow root-owned mutation authority used by the source-edit application.
///
/// Graph evidence is supplied separately as one admitted, generation-pinned
/// [`SourceEditGraphReadV1`]. This port owns only the edit primitives and
/// optional post-edit diagnostics; durable recovery and rollback are owned by
/// `tracedecay-source-edit`, and it must not grow legacy graph-query methods.
pub trait SourceEditRuntimePort: Send + Sync {
    fn project_root(&self) -> &Path;
    fn store_layout(&self) -> &StoreLayout;
    fn run_diagnostics<'a>(&'a self, file: &'a str) -> GraphFuture<'a, Vec<EditDiagnosticRecord>>;
    fn replace_symbol<'a>(
        &'a self,
        graph: SourceEditGraphReadV1,
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
        graph: SourceEditGraphReadV1,
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
        graph: SourceEditGraphReadV1,
        symbol: &'a str,
        dest_file: &'a str,
        dry_run: bool,
        update_references: bool,
    ) -> GraphFuture<'a, MoveResult>;
    fn rename_symbol<'a>(
        &'a self,
        graph: SourceEditGraphReadV1,
        binding: &'a RenameSymbolBindingV1,
        new_name: &'a str,
        dry_run: bool,
    ) -> GraphFuture<'a, RenameResult>;
}

pub type SourceEditRuntime = dyn SourceEditRuntimePort;

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

#[hotpath::measure(label = "usecases.edit.plan", future = true)]
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

#[hotpath::measure(label = "usecases.edit.apply", future = true)]
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
) -> bool {
    SOURCE_EDIT_PLAN_CAPTURE
        .try_with(|capture| {
            capture
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(PlannedSourceEditFile {
                    relative_path: relative_path.to_owned(),
                    expected: expected.map(str::to_owned),
                    intended: intended.map(str::to_owned),
                });
        })
        .is_ok()
}

#[hotpath::measure(label = "usecases.edit.validate")]
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
                return Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!(
                        "source edit apply produced unplanned candidate {relative_path}"
                    ),
                });
            };
            if planned.expected.as_deref() != expected || planned.intended.as_deref() != intended {
                return Err(tracedecay_domain::errors::TraceDecayError::Config {
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

pub struct SyncLockGuard(File);

impl Drop for SyncLockGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

#[hotpath::measure(label = "usecases.edit.lock")]
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
        tracedecay_domain::errors::TraceDecayError::SyncLock {
            message: format!("could not lock sync lockfile: {error}"),
        }
    })?;
    Ok(SyncLockGuard(file))
}
