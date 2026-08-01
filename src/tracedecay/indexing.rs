//! Full indexing, incremental sync, reference resolution, and staleness
//! detection for the active project store.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
#[cfg(any(test, feature = "test-transport"))]
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rayon::prelude::*;

use crate::config::brand_env;
use crate::errors::{Result, TraceDecayError};
use crate::resolution::ReferenceResolver;
use crate::sync;
use crate::types::*;

use super::{IndexResult, SyncResult, TraceDecay, current_timestamp};

const MIGRATION_REINDEX_STATE_KEY: &str = "migration_reindex_state_v1";
const MIGRATION_REINDEX_CHECKPOINT_DIR: &str = "migration-reindex-checkpoint-v1";
const MIGRATION_REINDEX_CHECKPOINT_BATCH_SIZE: usize = 1_024;
static MIGRATION_REINDEX_WORKERS: LazyLock<Mutex<HashSet<PathBuf>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
#[cfg(any(test, feature = "test-transport"))]
static MIGRATION_REINDEX_TEST_ABORT_AFTER_BATCHES: AtomicUsize = AtomicUsize::new(0);
#[cfg(any(test, feature = "test-transport"))]
static MIGRATION_REINDEX_TEST_EXTRACTIONS: AtomicUsize = AtomicUsize::new(0);
#[cfg(any(test, feature = "test-transport"))]
static MIGRATION_REINDEX_TEST_HOLD_LEASE_MS: AtomicU64 = AtomicU64::new(0);
#[cfg(any(test, feature = "test-transport"))]
static MIGRATION_REINDEX_TEST_DISABLE_SPAWN: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
struct MigrationReindexCheckpointEntryV1 {
    file_path: String,
    result: ExtractionResult,
    content_hash: String,
    size: u64,
    modified_at: i64,
}

impl From<ExtractTuple> for MigrationReindexCheckpointEntryV1 {
    fn from((file_path, result, content_hash, size, modified_at): ExtractTuple) -> Self {
        Self {
            file_path,
            result,
            content_hash,
            size,
            modified_at,
        }
    }
}

impl MigrationReindexCheckpointEntryV1 {
    fn into_extraction(self) -> ExtractTuple {
        (
            self.file_path,
            self.result,
            self.content_hash,
            self.size,
            self.modified_at,
        )
    }
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct MigrationReindexCheckpointBatchV1 {
    schema_version: u32,
    entries: Vec<MigrationReindexCheckpointEntryV1>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationReindexAvailabilityV1 {
    Stale,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum MigrationReindexStatusV1 {
    Current {
        schema_version: u32,
        completed_at: i64,
    },
    Indexing {
        schema_version: u32,
        completed_files: u64,
        total_files: Option<u64>,
        availability: MigrationReindexAvailabilityV1,
    },
    Failed {
        schema_version: u32,
        availability: MigrationReindexAvailabilityV1,
        retryable: bool,
        reason: String,
    },
}

fn should_schedule_migration_reindex(status: &MigrationReindexStatusV1) -> bool {
    matches!(
        status,
        MigrationReindexStatusV1::Indexing { .. }
            | MigrationReindexStatusV1::Failed {
                retryable: true,
                ..
            }
    )
}

struct MigrationReindexWorkerRegistration {
    key: PathBuf,
}

impl Drop for MigrationReindexWorkerRegistration {
    fn drop(&mut self) {
        MIGRATION_REINDEX_WORKERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.key);
    }
}

fn migration_reindex_checkpoint_error(
    operation: &str,
    error: impl std::fmt::Display,
) -> TraceDecayError {
    TraceDecayError::Database {
        operation: format!("{operation} migration re-index checkpoint"),
        message: error.to_string(),
    }
}

/// Convert any backslash in a *relative* project-root-relative path to a
/// forward slash, matching the canonical form the walker
/// ([`scan_files`](TraceDecay::scan_files) → [`accept_file`](TraceDecay::accept_file))
/// uses when writing to the DB.
///
/// Applied defensively at sync/staleness entry points so that callers
/// holding OS-native paths (PowerShell-shaped `src\foo.py`, paths echoed
/// back from MCP tool responses on Windows, etc.) hit the same `files`
/// row as the walker would — preventing the duplicate-row corruption
/// from #87 where the same physical file showed up as both `src/foo.py`
/// and `src\foo.py` in the `files` table.
fn normalize_rel_path(path: &str) -> String {
    path.replace('\\', "/")
}

/// Normalize a slice of relative paths to canonical (forward-slash)
/// form. Allocates a new `Vec` only when at least one entry needed
/// normalization — common case on Unix is a zero-copy pass-through to
/// the caller's existing `Vec`.
fn normalize_rel_paths(paths: &[String]) -> Vec<String> {
    paths.iter().map(|p| normalize_rel_path(p)).collect()
}

fn is_tracedecay_state_path(path: &str) -> bool {
    path == crate::config::TRACEDECAY_DIR
        || path
            .strip_prefix(crate::config::TRACEDECAY_DIR)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn is_safe_lazy_dependency_path(path: &str) -> bool {
    if !path.starts_with("node_modules/") || path.contains('\\') {
        return false;
    }
    Path::new(path)
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
}

/// Metadata flag recording that the *complete* extracted reference set has
/// been persisted to `unresolved_refs`. Indexes built before refs were
/// persisted (older `index_all`, which resolved in memory only) lack this key,
/// so the first sync after upgrading re-extracts every file once to rebuild
/// the full set and self-heal cross-file edges dropped on earlier edits (#1).
const UNRESOLVED_REFS_PERSISTED_KEY: &str = "unresolved_refs_persisted";
/// Marker value for [`UNRESOLVED_REFS_PERSISTED_KEY`]. Bump this string if a
/// future change requires the ref set to be repopulated again on upgrade.
const UNRESOLVED_REFS_PERSISTED_VALUE: &str = "1";
const BRANCH_SYNC_WRITE_PAGE_ROWS: usize = 256;
const BRANCH_SYNC_EXTRACTION_PAGE_FILES: usize = 8;

fn release_process_allocator_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: `malloc_trim` is a process-wide, thread-safe glibc allocator
        // maintenance operation. It does not invalidate live allocations.
        unsafe {
            libc::malloc_trim(0);
        }
    }
}

/// The final `::`-separated segment of a reference name. Every resolver
/// strategy ultimately binds a reference to a target whose short name equals
/// this segment, so it is the key used to scope incremental re-resolution.
fn simple_ref_name(reference_name: &str) -> &str {
    reference_name.rsplit("::").next().unwrap_or(reference_name)
}

/// Adds a qualified name and each of its `::` suffixes to `keys`, mirroring how
/// [`ReferenceResolver`] indexes nodes. Used to scope re-resolution to the
/// reference spellings that could bind to a re-indexed file's symbols.
fn add_resolver_keys(keys: &mut HashSet<String>, qualified_name: &str) {
    keys.insert(qualified_name.to_string());
    let mut pos = 0;
    while let Some(idx) = qualified_name[pos..].find("::") {
        let suffix = &qualified_name[pos + idx + 2..];
        if !suffix.is_empty() {
            keys.insert(suffix.to_string());
        }
        pos += idx + 2;
    }
}

/// Accumulates the short names and resolver keys of `nodes` (skipping `Use`
/// nodes, which the resolver also ignores) into the scope sets used to filter
/// references for incremental re-resolution.
pub(super) fn accumulate_symbol_scope(
    nodes: &[Node],
    short: &mut HashSet<String>,
    keys: &mut HashSet<String>,
) {
    for node in nodes {
        if node.kind == NodeKind::Use {
            continue;
        }
        short.insert(node.name.clone());
        add_resolver_keys(keys, &node.qualified_name);
    }
}

/// Builds the `(short names, resolver keys)` scope from re-indexed extraction
/// tuples — the symbols whose (re)definition can create or drop cross-file
/// edges during an incremental sync.
fn reindexed_symbol_scope(extractions: &[ExtractTuple]) -> (HashSet<String>, HashSet<String>) {
    let mut short = HashSet::new();
    let mut keys = HashSet::new();
    for (_path, result, _hash, _size, _mtime) in extractions {
        accumulate_symbol_scope(&result.nodes, &mut short, &mut keys);
    }
    (short, keys)
}

/// Run `extractor.extract()` inside `catch_unwind` so a panic (e.g. from a
/// malformed file or an extractor bug) skips the file instead of aborting sync.
pub(super) fn safe_extract(
    extractor: &dyn tracedecay_code_extraction::LanguageExtractor,
    file_path: &str,
    source: &str,
) -> Option<ExtractionResult> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        extractor.extract(file_path, source)
    }))
    .map_err(|_| {
        eprintln!("[tracedecay] extraction panicked for {file_path}, skipping");
    })
    .ok()
}

/// Tuple shape produced per file by both extraction paths.
type ExtractTuple = (String, ExtractionResult, String, u64, i64);

const MAX_EXTRACTION_WORKERS: usize = 8;

fn bounded_extraction_workers(available: usize, file_count: usize) -> usize {
    available
        .clamp(1, MAX_EXTRACTION_WORKERS)
        .min(file_count.max(1))
}

/// Extract every file in `files`, isolating each extraction in a subprocess
/// when possible. Subprocess isolation contains C/C++ grammar aborts that
/// `catch_unwind` cannot intercept; it is the primary defense against
/// tree-sitter scanners that call `abort()` (issue #49).
///
/// Falls back to in-process extraction with `safe_extract` if the worker
/// pool cannot start (e.g. when running under `cargo test`, where
/// `current_exe()` points at the test harness rather than the tracedecay
/// binary). Either way, returns one tuple per successfully-processed file
/// plus a list of `(path, reason)` pairs for files that timed out or
/// repeatedly crashed during extraction.
fn extract_files_isolated(
    project_root: &Path,
    registry: &tracedecay_code_extraction::LanguageRegistry,
    files: Vec<String>,
) -> (Vec<ExtractTuple>, Vec<(String, String)>) {
    if files.is_empty() {
        return (Vec::new(), Vec::new());
    }
    if should_use_subprocess() {
        let available = std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get);
        let workers = bounded_extraction_workers(available, files.len());
        let timeout = std::time::Duration::from_secs(
            crate::user_config::UserConfig::load().extraction_timeout_secs,
        );
        match crate::extraction_worker::WorkerPool::new(workers, project_root.to_path_buf()) {
            Ok(pool) => {
                let outcome = pool.extract_files(files, |_, _, _| {}, timeout);
                return (outcome.results, outcome.skipped);
            }
            Err(e) => eprintln!(
                "[tracedecay] could not spawn extraction worker pool ({e}), \
                 falling back to in-process extraction"
            ),
        }
    }
    (
        extract_files_in_process(project_root, registry, &files),
        Vec::new(),
    )
}

fn extract_files_in_process(
    project_root: &Path,
    registry: &tracedecay_code_extraction::LanguageRegistry,
    files: &[String],
) -> Vec<ExtractTuple> {
    files
        .par_iter()
        .filter_map(|file_path| {
            let abs_path = project_root.join(file_path);
            let source = sync::read_source_file(&abs_path).ok()?;
            let extractor = registry.extractor_for_file(file_path)?;
            let mut result = safe_extract(extractor, file_path, &source)?;
            result.sanitize();
            let hash = sync::content_hash(&source);
            let size = source.len() as u64;
            let mtime = sync::file_stat(&abs_path).map_or_else(current_timestamp, |(m, _)| m);
            Some((file_path.clone(), result, hash, size, mtime))
        })
        .collect()
}

/// Subprocess extraction is the production path. Tests and any environment
/// where `current_exe()` does not point at the real `tracedecay` binary
/// transparently fall back to in-process extraction.
fn should_use_subprocess() -> bool {
    if brand_env("DISABLE_SUBPROCESS").is_some() {
        return false;
    }
    let Ok(path) = std::env::current_exe() else {
        return false;
    };
    matches!(
        path.file_stem().and_then(|s| s.to_str()),
        Some("tracedecay")
    )
}

impl TraceDecay {
    #[cfg(any(test, feature = "test-transport"))]
    #[doc(hidden)]
    pub fn configure_migration_reindex_for_test(
        abort_after_batches: usize,
        hold_lease_ms: u64,
        disable_spawn: bool,
    ) {
        MIGRATION_REINDEX_TEST_ABORT_AFTER_BATCHES.store(abort_after_batches, Ordering::SeqCst);
        MIGRATION_REINDEX_TEST_HOLD_LEASE_MS.store(hold_lease_ms, Ordering::SeqCst);
        MIGRATION_REINDEX_TEST_DISABLE_SPAWN.store(disable_spawn, Ordering::SeqCst);
        MIGRATION_REINDEX_TEST_EXTRACTIONS.store(0, Ordering::SeqCst);
    }

    #[cfg(any(test, feature = "test-transport"))]
    #[doc(hidden)]
    pub fn migration_reindex_extractions_for_test() -> usize {
        MIGRATION_REINDEX_TEST_EXTRACTIONS.load(Ordering::SeqCst)
    }

    pub async fn migration_reindex_status(&self) -> Result<MigrationReindexStatusV1> {
        let Some(encoded) = self.db.get_metadata(MIGRATION_REINDEX_STATE_KEY).await? else {
            let generation_is_current = self
                .db
                .get_metadata(crate::db::migrations::GRAPH_GENERATION_SCHEMA_KEY)
                .await?
                .and_then(|value| value.parse::<u32>().ok())
                == Some(crate::db::migrations::LATEST_VERSION);
            if !generation_is_current {
                return Ok(MigrationReindexStatusV1::Indexing {
                    schema_version: crate::db::migrations::LATEST_VERSION,
                    completed_files: 0,
                    total_files: None,
                    availability: self.migration_reindex_availability().await?,
                });
            }
            let completed_at = self
                .db
                .get_metadata("last_full_sync_at")
                .await?
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            return Ok(MigrationReindexStatusV1::Current {
                schema_version: crate::db::migrations::LATEST_VERSION,
                completed_at,
            });
        };
        match serde_json::from_str(&encoded) {
            Ok(status) => Ok(status),
            Err(_) => Ok(MigrationReindexStatusV1::Failed {
                schema_version: crate::db::migrations::LATEST_VERSION,
                availability: self.migration_reindex_availability().await?,
                retryable: true,
                reason: "stored migration re-index state is unreadable".to_owned(),
            }),
        }
    }

    pub(super) async fn mark_migration_reindex_pending(&self) -> Result<()> {
        let availability = self.migration_reindex_availability().await?;
        self.write_migration_reindex_status(&MigrationReindexStatusV1::Indexing {
            schema_version: crate::db::migrations::LATEST_VERSION,
            completed_files: 0,
            total_files: None,
            availability,
        })
        .await
    }

    async fn migration_reindex_availability(&self) -> Result<MigrationReindexAvailabilityV1> {
        Ok(
            if self
                .db
                .query_scalar_i64(
                    "inspect pre-migration graph availability",
                    "SELECT EXISTS(SELECT 1 FROM files LIMIT 1)",
                )
                .await?
                != 0
            {
                MigrationReindexAvailabilityV1::Stale
            } else {
                MigrationReindexAvailabilityV1::Unavailable
            },
        )
    }

    pub(super) async fn schedule_migration_reindex_if_needed(&self) -> Result<()> {
        let status = self.migration_reindex_status().await?;
        if !should_schedule_migration_reindex(&status) {
            return Ok(());
        }
        if let Err(error) = self.ensure_branch_writable("schedule migration re-index") {
            eprintln!(
                "[tracedecay] migration re-index remains pending on a read-only branch: {error}"
            );
            return Ok(());
        }
        #[cfg(any(test, feature = "test-transport"))]
        if MIGRATION_REINDEX_TEST_DISABLE_SPAWN.load(Ordering::SeqCst) {
            return Ok(());
        }
        let key = self.active_graph_layout.sync_lock_path.clone();
        if !MIGRATION_REINDEX_WORKERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key.clone())
        {
            return Ok(());
        }
        let availability = match status {
            MigrationReindexStatusV1::Indexing { availability, .. }
            | MigrationReindexStatusV1::Failed { availability, .. } => availability,
            MigrationReindexStatusV1::Current { .. } => return Ok(()),
        };
        let worker = self.clone_for_background_index();
        tokio::spawn(async move {
            let _registration = MigrationReindexWorkerRegistration { key };
            worker.run_migration_reindex(availability).await;
        });
        Ok(())
    }

    async fn run_migration_reindex(self, availability: MigrationReindexAvailabilityV1) {
        let total = u64::try_from(self.scan_files().len()).unwrap_or(u64::MAX);
        if let Err(error) = self
            .write_migration_reindex_status(&MigrationReindexStatusV1::Indexing {
                schema_version: crate::db::migrations::LATEST_VERSION,
                completed_files: 0,
                total_files: Some(total),
                availability,
            })
            .await
        {
            eprintln!("[tracedecay] could not checkpoint migration re-index state: {error}");
            return;
        }

        eprintln!("[tracedecay] schema changed — rebuilding {total} files in the background");
        let result = self
            .index_all_with_migration_checkpoint(availability, |current, total, file| {
                if current == total || current % 250 == 0 {
                    eprintln!("[tracedecay] background re-index [{current}/{total}] {file}");
                }
            })
            .await;
        match result {
            Ok(_) => {
                eprintln!("[tracedecay] background migration re-index complete");
            }
            Err(TraceDecayError::SyncLock { message })
                if message.contains("another sync is already in progress") =>
            {
                eprintln!(
                    "[tracedecay] background migration re-index deferred while another sync is active"
                );
            }
            Err(error) => {
                eprintln!("[tracedecay] background migration re-index failed: {error}");
                let status = MigrationReindexStatusV1::Failed {
                    schema_version: crate::db::migrations::LATEST_VERSION,
                    availability,
                    retryable: true,
                    reason: error.to_string(),
                };
                if let Err(error) = self.write_migration_reindex_status(&status).await {
                    eprintln!("[tracedecay] could not publish migration re-index state: {error}");
                }
            }
        }
    }

    async fn write_migration_reindex_status(
        &self,
        status: &MigrationReindexStatusV1,
    ) -> Result<()> {
        let encoded = serde_json::to_string(status).map_err(|error| TraceDecayError::Database {
            operation: "write migration re-index state".to_owned(),
            message: error.to_string(),
        })?;
        self.db
            .set_metadata(MIGRATION_REINDEX_STATE_KEY, &encoded)
            .await
    }

    async fn index_all_with_migration_checkpoint<F>(
        &self,
        availability: MigrationReindexAvailabilityV1,
        on_file: F,
    ) -> Result<IndexResult>
    where
        F: Fn(usize, usize, &str),
    {
        let result = self
            .index_all_with_progress_verbose_mode(on_file, |_| {}, Some(availability))
            .await;
        if result.is_ok() {
            let checkpoint_root = self.migration_reindex_checkpoint_root();
            match std::fs::remove_dir_all(&checkpoint_root) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => eprintln!(
                    "[tracedecay] could not remove completed migration checkpoint '{}': {error}",
                    checkpoint_root.display()
                ),
            }
        }
        result
    }

    async fn migration_reindex_extractions(
        &self,
        files: &[String],
        availability: MigrationReindexAvailabilityV1,
    ) -> Result<Vec<ExtractTuple>> {
        let checkpoint_root = self.migration_reindex_checkpoint_root();
        let (mut completed, mut sequence) =
            self.load_migration_reindex_checkpoints(&checkpoint_root)?;
        let current_files = files.iter().cloned().collect::<HashSet<_>>();
        completed.retain(|path, entry| {
            current_files.contains(path)
                && sync::read_source_file(&self.project_root.join(path))
                    .ok()
                    .is_some_and(|source| sync::content_hash(&source) == entry.content_hash)
        });

        let pending = files
            .iter()
            .filter(|path| !completed.contains_key(*path))
            .cloned()
            .collect::<Vec<_>>();
        for (batch_index, batch) in pending
            .chunks(MIGRATION_REINDEX_CHECKPOINT_BATCH_SIZE)
            .enumerate()
        {
            #[cfg(not(any(test, feature = "test-transport")))]
            let _ = batch_index;
            let (extractions, _skipped) =
                extract_files_isolated(&self.project_root, &self.registry, batch.to_vec());
            #[cfg(any(test, feature = "test-transport"))]
            MIGRATION_REINDEX_TEST_EXTRACTIONS.fetch_add(extractions.len(), Ordering::SeqCst);
            let entries = extractions
                .into_iter()
                .map(MigrationReindexCheckpointEntryV1::from)
                .collect::<Vec<_>>();
            sequence = sequence.saturating_add(1);
            self.write_migration_reindex_checkpoint_batch(&checkpoint_root, sequence, &entries)?;
            for entry in entries {
                completed.insert(entry.file_path.clone(), entry);
            }
            self.write_migration_reindex_status(&MigrationReindexStatusV1::Indexing {
                schema_version: crate::db::migrations::LATEST_VERSION,
                completed_files: u64::try_from(completed.len()).unwrap_or(u64::MAX),
                total_files: Some(u64::try_from(files.len()).unwrap_or(u64::MAX)),
                availability,
            })
            .await?;
            #[cfg(any(test, feature = "test-transport"))]
            if MIGRATION_REINDEX_TEST_ABORT_AFTER_BATCHES.load(Ordering::SeqCst) == batch_index + 1
            {
                return Err(TraceDecayError::Database {
                    operation: "test migration re-index interruption".to_owned(),
                    message: format!("aborted after {} checkpoint batch(es)", batch_index + 1),
                });
            }
        }

        Ok(files
            .iter()
            .filter_map(|path| completed.remove(path))
            .map(MigrationReindexCheckpointEntryV1::into_extraction)
            .collect())
    }

    fn load_migration_reindex_checkpoints(
        &self,
        checkpoint_root: &Path,
    ) -> Result<(BTreeMap<String, MigrationReindexCheckpointEntryV1>, u64)> {
        let entries = match std::fs::read_dir(checkpoint_root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((BTreeMap::new(), 0));
            }
            Err(error) => return Err(migration_reindex_checkpoint_error("list", error)),
        };
        let mut paths = entries
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension().and_then(|extension| extension.to_str()) == Some("json")
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("batch-"))
            })
            .collect::<Vec<_>>();
        paths.sort();

        let mut completed = BTreeMap::new();
        let mut sequence = 0_u64;
        for path in paths {
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    eprintln!(
                        "[tracedecay] ignoring unreadable migration checkpoint '{}': {error}",
                        path.display()
                    );
                    continue;
                }
            };
            let batch: MigrationReindexCheckpointBatchV1 = match serde_json::from_slice(&bytes) {
                Ok(batch) => batch,
                Err(error) => {
                    eprintln!(
                        "[tracedecay] ignoring malformed migration checkpoint '{}': {error}",
                        path.display()
                    );
                    continue;
                }
            };
            if batch.schema_version != crate::db::migrations::LATEST_VERSION {
                continue;
            }
            sequence = sequence.max(
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .and_then(|stem| stem.strip_prefix("batch-"))
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0),
            );
            for entry in batch.entries {
                completed.insert(entry.file_path.clone(), entry);
            }
        }
        Ok((completed, sequence))
    }

    fn write_migration_reindex_checkpoint_batch(
        &self,
        checkpoint_root: &Path,
        sequence: u64,
        entries: &[MigrationReindexCheckpointEntryV1],
    ) -> Result<()> {
        let batch = MigrationReindexCheckpointBatchV1 {
            schema_version: crate::db::migrations::LATEST_VERSION,
            entries: entries.to_vec(),
        };
        let bytes = serde_json::to_vec(&batch).map_err(|error| TraceDecayError::Database {
            operation: "write migration re-index checkpoint".to_owned(),
            message: error.to_string(),
        })?;
        let path = checkpoint_root.join(format!("batch-{sequence:020}.json"));
        let temporary =
            checkpoint_root.join(format!(".batch-{sequence:020}.{}.tmp", std::process::id()));
        crate::storage::PrivateStoreIo::write_file_atomically_durable(&path, &temporary, &bytes)
            .map_err(|error| migration_reindex_checkpoint_error("write", error))
    }

    fn migration_reindex_checkpoint_root(&self) -> PathBuf {
        self.active_graph_layout
            .sync_lock_path
            .with_extension(MIGRATION_REINDEX_CHECKPOINT_DIR)
    }

    fn clone_for_background_index(&self) -> Self {
        Self {
            db: self.db.clone(),
            profile_database: Arc::clone(&self.profile_database),
            store_runtime_registry: Arc::clone(&self.store_runtime_registry),
            config: self.config.clone(),
            configuration_runtime: Arc::clone(&self.configuration_runtime),
            project_root: self.project_root.clone(),
            store_layout: self.store_layout.clone(),
            active_graph_layout: self.active_graph_layout.clone(),
            open_options: self.open_options.clone(),
            registry: tracedecay_code_extraction::LanguageRegistry::new(),
            active_branch: self.active_branch.clone(),
            serving_branch: self.serving_branch.clone(),
            fallback_warning: self.fallback_warning.clone(),
            read_only: self.read_only,
            db_path_cache: OnceLock::new(),
            context_scout_owner: self.context_scout_owner.clone(),
            context_scout_claim_authorities: tokio::sync::RwLock::default(),
            #[cfg(any(test, feature = "test-transport"))]
            test_runtime_guard: self.test_runtime_guard.clone(),
            standalone_maintenance_scope: self.standalone_maintenance_scope.clone(),
        }
    }

    /// Performs a full index: clears existing data, scans all Rust files,
    /// extracts nodes and edges, resolves references, and stores everything
    /// in the database.
    pub async fn index_all(&self) -> Result<IndexResult> {
        self.index_all_with_progress(|_, _, _| {}).await
    }

    /// Like `index_all()`, but calls `on_file(current, total, path)` before
    /// processing each file. Use this to drive a progress spinner with ETA in
    /// the CLI.
    pub async fn index_all_with_progress<F>(&self, on_file: F) -> Result<IndexResult>
    where
        F: Fn(usize, usize, &str),
    {
        self.index_all_with_progress_verbose(on_file, |_| {}).await
    }

    /// Like `index_all_with_progress()`, but also calls `on_verbose` after
    /// each phase completes with a diagnostic summary line.
    pub async fn index_all_with_progress_verbose<F, V>(
        &self,
        on_file: F,
        on_verbose: V,
    ) -> Result<IndexResult>
    where
        F: Fn(usize, usize, &str),
        V: Fn(&str),
    {
        self.index_all_with_progress_verbose_mode(on_file, on_verbose, None)
            .await
    }

    async fn index_all_with_progress_verbose_mode<F, V>(
        &self,
        on_file: F,
        on_verbose: V,
        migration_availability: Option<MigrationReindexAvailabilityV1>,
    ) -> Result<IndexResult>
    where
        F: Fn(usize, usize, &str),
        V: Fn(&str),
    {
        debug_assert!(self.project_root.exists(), "project root does not exist");
        debug_assert!(
            self.project_root.is_dir(),
            "project root is not a directory"
        );
        // R4: one branch resolution for the whole full index — the write gate
        // and the closing branch-meta stamp share it.
        let live_branch = self.branch_memo();
        self.ensure_branch_writable_with("full index", &live_branch)?;
        let sync_lease = self.begin_active_sync()?;
        #[cfg(any(test, feature = "test-transport"))]
        if migration_availability.is_some() {
            let hold_ms = MIGRATION_REINDEX_TEST_HOLD_LEASE_MS.load(Ordering::SeqCst);
            if hold_ms > 0 {
                tokio::time::sleep(Duration::from_millis(hold_ms)).await;
            }
        }
        let start = Instant::now();

        // 1. Scan for source files
        let phase_start = Instant::now();
        let files = self.scan_files();
        let total = files.len();
        on_verbose(&format!(
            "scanned {} files in {:.1}s",
            total,
            phase_start.elapsed().as_secs_f64()
        ));

        // 2. Parallel extraction: read + parse + hash on all cores
        let project_root = self.project_root.clone();
        let registry = &self.registry;

        let phase_start = Instant::now();
        let extractions = if let Some(availability) = migration_availability {
            self.migration_reindex_extractions(&files, availability)
                .await?
        } else {
            extract_files_isolated(&project_root, registry, files.clone()).0
        };

        // 3. Collect all data
        let mut all_nodes = Vec::new();
        let mut all_edges = Vec::new();
        let mut all_unresolved = Vec::new();
        let mut file_records = Vec::new();
        let mut total_nodes = 0;

        for (idx, (file_path, result, hash, size, mtime)) in extractions.iter().enumerate() {
            on_file(idx + 1, total, file_path);
            total_nodes += result.nodes.len();
            all_nodes.extend_from_slice(&result.nodes);
            all_edges.extend_from_slice(&result.edges);
            all_unresolved.extend_from_slice(&result.unresolved_refs);
            file_records.push(FileRecord {
                path: file_path.clone(),
                content_hash: hash.clone(),
                size: *size,
                modified_at: *mtime,
                indexed_at: current_timestamp(),
                node_count: result.nodes.len() as u32,
            });
        }

        on_verbose(&format!(
            "extracted {} nodes, {} edges from {} files in {:.1}s",
            total_nodes,
            all_edges.len(),
            extractions.len(),
            phase_start.elapsed().as_secs_f64()
        ));

        // 4. Resolve references in-memory (parallel) before DB insert
        let phase_start = Instant::now();
        if !all_unresolved.is_empty() {
            let resolver = ReferenceResolver::from_nodes(&self.db, &all_nodes);
            let resolution = resolver.resolve_all(&all_unresolved);
            all_edges.extend(resolver.create_edges(&resolution.resolved));
        }
        on_verbose(&format!(
            "resolved {} references in {:.1}s",
            all_unresolved.len(),
            phase_start.elapsed().as_secs_f64()
        ));

        // 5. Sort by PK order + dedup edges
        all_nodes.sort_unstable_by(|a, b| a.id.cmp(&b.id));
        all_edges.sort_unstable_by(|a, b| {
            (&a.source, &a.target, a.kind.as_str(), &a.line).cmp(&(
                &b.source,
                &b.target,
                b.kind.as_str(),
                &b.line,
            ))
        });
        all_edges.dedup_by(|a, b| {
            a.source == b.source && a.target == b.target && a.kind == b.kind && a.line == b.line
        });
        file_records.sort_unstable_by(|a, b| a.path.cmp(&b.path));
        let total_edges = all_edges.len();

        // 6. Replace the index under one writer lane. CPU-heavy scanning,
        // extraction, and reference resolution above do not block unrelated
        // database mutations while the connection is still in normal mode.
        let transaction = self
            .db
            .begin_bulk_write_transaction("replace full index")
            .await?;
        self.db.clear_unguarded(&transaction).await?;
        self.db.begin_bulk_load_unguarded(&transaction).await?;

        // 7. Bulk-insert via prepared statements (zero SQL re-parsing)
        let phase_start = Instant::now();
        self.db
            .insert_nodes_unguarded(&transaction, &all_nodes)
            .await?;
        // Persist the full extracted reference set (after nodes exist for the
        // FK). Later incremental syncs re-resolve from this set to rebuild
        // cross-file edges into files they re-index; without it, editing a
        // symbol deletes unchanged callers' edges (which cascade off the
        // target node) and they can never be resolved again (#1).
        if !all_unresolved.is_empty() {
            self.db
                .insert_unresolved_refs_unguarded(&transaction, &all_unresolved)
                .await?;
        }
        self.db
            .insert_edges_unguarded(&transaction, &all_edges)
            .await?;
        self.db
            .upsert_files_unguarded(&transaction, &file_records)
            .await?;

        // 8. Restore indexes and normal durability
        self.db.end_bulk_load_unguarded(&transaction).await?;
        on_verbose(&format!(
            "wrote to database in {:.1}s",
            phase_start.elapsed().as_secs_f64()
        ));

        let duration_ms = start.elapsed().as_millis() as u64;
        let now = current_timestamp();
        let now_str = now.to_string();
        self.db
            .set_metadata_unguarded(&transaction, "last_full_sync_at", &now_str)
            .await?;
        self.db
            .set_metadata_unguarded(&transaction, "last_sync_at", &now_str)
            .await?;
        self.db
            .set_metadata_unguarded(
                &transaction,
                "last_sync_duration_ms",
                &duration_ms.to_string(),
            )
            .await?;
        // The unresolved_refs table now holds the complete extracted set, so
        // incremental syncs can self-heal without re-extracting everything.
        self.db
            .set_metadata_unguarded(
                &transaction,
                UNRESOLVED_REFS_PERSISTED_KEY,
                UNRESOLVED_REFS_PERSISTED_VALUE,
            )
            .await?;
        self.db
            .set_metadata_unguarded(
                &transaction,
                crate::db::migrations::GRAPH_GENERATION_SCHEMA_KEY,
                &crate::db::migrations::LATEST_VERSION.to_string(),
            )
            .await?;
        if migration_availability.is_some() {
            let status = serde_json::to_string(&MigrationReindexStatusV1::Current {
                schema_version: crate::db::migrations::LATEST_VERSION,
                completed_at: now,
            })
            .map_err(|error| TraceDecayError::Database {
                operation: "publish migration re-index generation".to_owned(),
                message: error.to_string(),
            })?;
            self.db
                .set_metadata_unguarded(&transaction, MIGRATION_REINDEX_STATE_KEY, &status)
                .await?;
        }
        transaction.commit().await?;
        // Stamp HEAD after releasing the full-index transaction: this helper
        // acquires its own writer lane, as do the incremental-sync call sites.
        self.stamp_last_synced_commit().await;
        self.touch_branch_meta_synced(&live_branch);

        let result = IndexResult {
            file_count: files.len(),
            node_count: total_nodes,
            edge_count: total_edges,
            duration_ms,
        };
        debug_assert!(
            result.node_count >= result.file_count || result.file_count == 0,
            "fewer nodes than files is unexpected"
        );
        debug_assert!(
            result.duration_ms > 0 || result.file_count == 0,
            "non-empty index completed in zero milliseconds"
        );
        self.db.checkpoint().await?;
        sync_lease.commit()?;
        Ok(result)
    }

    /// Performs an incremental sync: detects changed, new, and removed files
    /// and re-indexes only those that need updating.
    pub async fn sync(&self) -> Result<SyncResult> {
        self.sync_with_progress(|_, _, _| {}).await
    }

    /// Like `sync()`, but calls `on_progress` for spinner updates.
    /// Equivalent to `sync_with_progress_verbose(on_progress, |_| {})`.
    pub async fn sync_with_progress<F>(&self, on_progress: F) -> Result<SyncResult>
    where
        F: Fn(usize, usize, &str),
    {
        self.sync_with_progress_verbose(on_progress, |_| {}).await
    }

    /// Sync only the specified files if they are stale, then recheck.
    ///
    /// Returns `Ok(false)` if all files are now in sync after the call.
    /// Returns `Ok(true)` if files are still stale after sync (either sync
    /// didn't update these specific files, or sync failed to acquire lock).
    /// Returns `Err` on sync failure.
    pub async fn sync_if_stale(&self, stale_files: &[String]) -> Result<bool> {
        if stale_files.is_empty() {
            return Ok(false);
        }
        // Normalize once at the entry; downstream helpers can rely on
        // forward-slash form matching the walker's canonical path
        // (defends against #87 — Windows duplicate-row corruption).
        let stale_files = normalize_rel_paths(stale_files);

        let still_stale_before = self.check_file_staleness(&stale_files).await;
        if still_stale_before.is_empty() {
            return Ok(false);
        }

        // R4: one branch resolution for this whole sync — the entry gate, the
        // inner `sync_single_files` gate, and the branch-meta stamp all read
        // it instead of re-opening the repository three times.
        let live_branch = self.branch_memo();
        self.ensure_branch_writable_with("sync files", &live_branch)?;

        let Ok(sync_lease) = self.begin_active_sync() else {
            return Ok(true);
        };

        let result = self.sync_single_files(&stale_files, &live_branch).await;

        match result {
            Ok(()) => {
                sync_lease.commit()?;
                let still_stale_after = self.check_file_staleness(&stale_files).await;
                Ok(!still_stale_after.is_empty())
            }
            Err(_) => Ok(true),
        }
    }

    /// Like `sync_if_stale` but treats lock contention as success.
    ///
    /// Use this from the embedded MCP watcher when another MCP (or any peer
    /// process) already holds the project sync lock. If the peer holds the
    /// lock, wait (bounded) for it to release so the DB is fresh by the time
    /// the caller refreshes its view; if the peer covered our files, return
    /// without doing extra work, otherwise sync ourselves.
    pub async fn sync_if_stale_silent(&self, stale_files: &[String]) -> Result<()> {
        if stale_files.is_empty() {
            return Ok(());
        }
        // Normalize once at the entry — see `sync_if_stale` and #87.
        let stale_files = normalize_rel_paths(stale_files);

        let still_stale_before = self.check_file_staleness(&stale_files).await;
        if still_stale_before.is_empty() {
            return Ok(());
        }

        // R4: one branch resolution threaded through the entry gate, the inner
        // `sync_single_files` gate, and the branch-meta stamp.
        let live_branch = self.branch_memo();
        self.ensure_branch_writable_with("sync files", &live_branch)?;

        let sync_lease = if let Ok(sync_lease) = self.begin_active_sync() {
            sync_lease
        } else {
            // Peer is syncing. Wait for them to release the lock so the
            // caller (e.g. the embedded watcher's refresh hook) sees the
            // post-sync DB state — returning early here leaves the caller
            // refreshing against pre-sync data and silently dropping the
            // update on the floor.
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                if Instant::now() >= deadline {
                    // Peer is stuck or crashed — best-effort, give up.
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
                if let Ok(sync_lease) = self.begin_active_sync() {
                    // Peer released. If they covered our files, the DB is
                    // fresh and we're done; otherwise sync ourselves.
                    let still_stale = self.check_file_staleness(&stale_files).await;
                    if still_stale.is_empty() {
                        sync_lease.commit()?;
                        return Ok(());
                    }
                    break sync_lease;
                }
            }
        };

        if self
            .sync_single_files(&stale_files, &live_branch)
            .await
            .is_ok()
        {
            sync_lease.commit()?;
        }
        Ok(())
    }

    /// Index/reexamine the given file paths, updating their graph nodes and edges.
    /// This is a focused, single-shot operation used by `sync_if_stale`.
    ///
    /// `live_branch` is the caller's per-request branch resolution; every
    /// public entry that reaches here already made one.
    async fn sync_single_files(
        &self,
        file_paths: &[String],
        live_branch: &crate::branch::BranchMemo,
    ) -> Result<()> {
        use crate::sync as sync_mod;

        self.ensure_branch_writable_with("sync files", live_branch)?;

        let start = Instant::now();
        let project_root = &self.project_root;
        let registry = &self.registry;

        // Defence-in-depth: even though the public `sync_if_stale[_silent]`
        // entry points already normalize, this is the single chokepoint
        // where paths get written to the DB — so we normalize again here
        // in case a future internal caller skips the wrappers. The DB's
        // canonical form is forward-slash (#87).
        let file_paths = normalize_rel_paths(file_paths);

        // Read and hash files that still exist. Missing targeted paths are
        // deletions, so clean their DB rows immediately instead of handing
        // them to extraction where they would be silently skipped.
        let mut existing_file_paths: Vec<String> = Vec::with_capacity(file_paths.len());
        let mut removed_file_paths: Vec<String> = Vec::new();
        let mut hash_map: HashMap<String, String> = HashMap::new();
        let mut stat_map: HashMap<String, (i64, u64)> = HashMap::new();

        for path in &file_paths {
            let abs_path = project_root.join(path);
            if let Some((mtime, size)) = sync_mod::file_stat(&abs_path) {
                stat_map.insert(path.clone(), (mtime, size));
                existing_file_paths.push(path.clone());
            } else {
                removed_file_paths.push(path.clone());
                continue;
            }
            if let Ok(source) = sync_mod::read_source_file(&abs_path) {
                let hash = sync_mod::content_hash(&source);
                hash_map.insert(path.clone(), hash);
            }
        }

        // Extract graph data from the files in parallel (subprocess-isolated)
        let _ = stat_map; // worker re-stats internally; map kept for potential future use
        let (sync_extractions, _skipped_extractions) =
            extract_files_isolated(project_root, registry, existing_file_paths.clone());
        let transaction = self
            .db
            .begin_write_transaction("sync selected files")
            .await?;

        for path in &removed_file_paths {
            self.db.delete_file_unguarded(&transaction, path).await?;
        }

        // Phase 1: insert all nodes (and metadata) so cross-file edges
        // can reference them. Edges are queued for phase 2 (#58).
        let mut queued_edges: Vec<&Edge> = Vec::new();
        for (file_path, result, hash, size, mtime) in &sync_extractions {
            self.db
                .delete_nodes_by_file_unguarded(&transaction, file_path)
                .await?;
            self.db
                .insert_nodes_unguarded(&transaction, &result.nodes)
                .await?;
            queued_edges.extend(&result.edges);
            if !result.unresolved_refs.is_empty() {
                self.db
                    .insert_unresolved_refs_unguarded(&transaction, &result.unresolved_refs)
                    .await?;
            }

            let file_record = FileRecord {
                path: (*file_path).clone(),
                content_hash: (*hash).clone(),
                size: *size,
                modified_at: *mtime,
                indexed_at: current_timestamp(),
                node_count: result.nodes.len() as u32,
            };
            self.db
                .upsert_file_unguarded(&transaction, &file_record)
                .await?;
        }

        // Phase 2: insert all queued edges now that every node is present.
        // The conditional INSERT in `insert_edges` silently skips edges
        // whose endpoints are truly missing (e.g. unindexed files).
        if !queued_edges.is_empty() {
            let owned: Vec<Edge> = queued_edges.into_iter().cloned().collect();
            self.db.insert_edges_unguarded(&transaction, &owned).await?;
        }

        transaction.commit().await?;

        // Resolve references for any new/changed unresolved refs, scoped to the
        // re-indexed files so we don't re-resolve the whole repo on each call.
        if !existing_file_paths.is_empty() {
            let (short, keys) = reindexed_symbol_scope(&sync_extractions);
            self.reresolve_after_reindex(&existing_file_paths, &short, &keys)
                .await?;
        }

        self.db
            .set_metadata("last_sync_at", &current_timestamp().to_string())
            .await?;
        // Advance the watcher's diff base even for targeted file syncs: if
        // HEAD is unchanged, re-stamping the same commit is idempotent; if a
        // hook-driven edit accompanied a commit, this keeps the base accurate.
        self.stamp_last_synced_commit().await;
        self.touch_branch_meta_synced(live_branch);
        self.db
            .set_metadata(
                "last_sync_duration_ms",
                &start.elapsed().as_millis().to_string(),
            )
            .await?;

        self.db.checkpoint().await?;
        Ok(())
    }

    /// Lazily index explicitly requested dependency files that normal sync
    /// skips, keeping the operation bounded and inside `node_modules`.
    pub async fn lazy_index_ignored_dependency_files(
        &self,
        file_paths: &[String],
    ) -> Result<Vec<String>> {
        // R4: one branch resolution for the entry gate and the inner
        // `sync_single_files` gate.
        let live_branch = self.branch_memo();
        self.ensure_branch_writable_with("lazy index ignored dependency files", &live_branch)?;

        let mut accepted = Vec::new();
        let mut seen = HashSet::new();
        for path in file_paths {
            let normalized = normalize_rel_path(path.trim());
            if !is_safe_lazy_dependency_path(&normalized) || !seen.insert(normalized.clone()) {
                continue;
            }
            let abs_path = self.project_root.join(&normalized);
            let Ok(metadata) = std::fs::metadata(&abs_path) else {
                continue;
            };
            if !metadata.is_file()
                || metadata.len() > self.config.max_file_size
                || self.registry.extractor_for_file(&normalized).is_none()
            {
                continue;
            }
            accepted.push(normalized);
            if accepted.len() >= 20 {
                break;
            }
        }

        if !accepted.is_empty() {
            let sync_lease = self.begin_active_sync()?;
            self.sync_single_files(&accepted, &live_branch).await?;
            sync_lease.commit()?;
        }
        Ok(accepted)
    }

    /// Re-resolves only the references whose edge can appear or disappear when
    /// `reindexed_files` are re-indexed: references originating in those files,
    /// and references anywhere whose target symbol is (re)defined in them.
    ///
    /// `delete_nodes_by_file` deletes every edge incident to a re-indexed file's
    /// nodes (source *or* target), so an unchanged caller's edge into an edited
    /// file is dropped on every sync; re-resolving these references rebuilds it
    /// without paying to re-resolve and re-insert the whole repository each time
    /// (#1). Completeness: every resolver strategy binds a reference to a target
    /// whose short name equals the reference's final `::` segment, so scoping on
    /// the re-indexed files' symbol short names (plus their qualified
    /// names/suffixes, and the re-indexed file paths) captures every reference
    /// that could resolve into or out of them. Immediately after a self-heal —
    /// which repopulates the full set — it resolves globally once instead.
    pub(super) async fn reresolve_after_reindex(
        &self,
        reindexed_files: &[String],
        reindexed_short: &HashSet<String>,
        reindexed_keys: &HashSet<String>,
    ) -> Result<()> {
        if self.heal_unresolved_refs_if_needed().await? {
            return self.resolve_all_unresolved_refs().await;
        }

        let unresolved = self.db.get_unresolved_refs().await?;
        let file_set: HashSet<&str> = reindexed_files.iter().map(String::as_str).collect();
        let scoped: Vec<UnresolvedRef> = unresolved
            .into_iter()
            .filter(|uref| {
                file_set.contains(uref.file_path.as_str())
                    || reindexed_short.contains(simple_ref_name(&uref.reference_name))
                    || reindexed_keys.contains(uref.reference_name.as_str())
            })
            .collect();
        self.resolve_refs(scoped).await
    }

    /// Global re-resolution over every persisted unresolved reference. Used
    /// after a self-heal (which repopulates the complete set) and as the safe
    /// fallback. The incremental path prefers
    /// [`reresolve_after_reindex`](Self::reresolve_after_reindex), which scopes
    /// work to the changed files.
    async fn resolve_all_unresolved_refs(&self) -> Result<()> {
        let unresolved = self.db.get_unresolved_refs().await?;
        self.resolve_refs(unresolved).await
    }

    /// Resolves a batch of references against the current graph and persists the
    /// resulting edges. Idempotent: `insert_edges` uses `INSERT OR IGNORE`, so
    /// re-resolving an already-present edge is a no-op.
    async fn resolve_refs(&self, refs: Vec<UnresolvedRef>) -> Result<()> {
        if refs.is_empty() {
            return Ok(());
        }
        let all_nodes = self.db.get_all_nodes().await.unwrap_or_default();
        let resolver = ReferenceResolver::from_nodes(&self.db, &all_nodes);
        let resolution = resolver.resolve_all(&refs);
        let edges = resolver.create_edges(&resolution.resolved);
        if !edges.is_empty() {
            self.db.insert_edges(&edges).await?;
        }
        Ok(())
    }

    /// Self-heals indexes built before unresolved refs were persisted.
    ///
    /// Older `index_all` resolved references in memory but never wrote them to
    /// `unresolved_refs`, so such an index is missing the refs of every file
    /// that has not changed since. Editing a symbol then permanently drops the
    /// unchanged callers' edges (they cascade off the deleted target node and
    /// can never be re-resolved). The first sync after upgrading re-extracts
    /// every indexed file once to rebuild the complete ref set, then records
    /// the [`UNRESOLVED_REFS_PERSISTED_KEY`] marker so this runs at most once.
    ///
    /// Returns `true` when it performed a heal (so the caller resolves globally
    /// once), `false` when the index was already complete.
    async fn heal_unresolved_refs_if_needed(&self) -> Result<bool> {
        let already_complete = self
            .db
            .get_metadata(UNRESOLVED_REFS_PERSISTED_KEY)
            .await?
            .as_deref()
            == Some(UNRESOLVED_REFS_PERSISTED_VALUE);
        if already_complete {
            return Ok(false);
        }

        let files: Vec<String> = self
            .db
            .get_all_files()
            .await?
            .into_iter()
            .map(|record| record.path)
            .collect();
        if !files.is_empty() {
            // Replace the incomplete persisted set with the freshly extracted
            // complete one. Clearing first also bounds unbounded ref growth (#4).
            self.db.clear_unresolved_refs().await?;
            for paths in files.chunks(BRANCH_SYNC_EXTRACTION_PAGE_FILES) {
                let (extractions, _skipped) =
                    extract_files_isolated(&self.project_root, &self.registry, paths.to_vec());
                let unresolved = extractions
                    .iter()
                    .flat_map(|(_, result, _, _, _)| result.unresolved_refs.iter().cloned())
                    .collect::<Vec<_>>();
                if !unresolved.is_empty() {
                    self.db.insert_unresolved_refs(&unresolved).await?;
                }
            }
        }
        self.db
            .set_metadata(
                UNRESOLVED_REFS_PERSISTED_KEY,
                UNRESOLVED_REFS_PERSISTED_VALUE,
            )
            .await?;
        Ok(true)
    }

    /// Like `sync()`, but calls `on_progress` with a description and the
    /// current step for each phase of work, and `on_verbose` after each phase
    /// completes with a diagnostic summary line (count + timing).
    ///
    /// The progress callback receives `(current_file_index, total_files, message)`
    /// where `current_file_index` and `total_files` are zero during non-file phases
    /// (scanning, hashing, detecting, resolving) and populated during the
    /// per-file syncing phase.
    pub async fn sync_with_progress_verbose<F, V>(
        &self,
        on_progress: F,
        on_verbose: V,
    ) -> Result<SyncResult>
    where
        F: Fn(usize, usize, &str),
        V: Fn(&str),
    {
        self.sync_with_progress_verbose_mode(on_progress, on_verbose, false)
            .await
    }

    pub(super) async fn sync_checkpointed(&self) -> Result<SyncResult> {
        self.sync_with_progress_verbose_mode(|_, _, _| {}, |_| {}, true)
            .await
    }

    async fn sync_with_progress_verbose_mode<F, V>(
        &self,
        on_progress: F,
        on_verbose: V,
        checkpoint_writes: bool,
    ) -> Result<SyncResult>
    where
        F: Fn(usize, usize, &str),
        V: Fn(&str),
    {
        debug_assert!(
            self.project_root.exists(),
            "sync: project root does not exist"
        );
        debug_assert!(
            self.project_root.is_dir(),
            "sync: project root is not a directory"
        );
        // R4: one branch resolution for the whole sync — the write gate and
        // both branch-meta stamp points below share it.
        let live_branch = self.branch_memo();
        self.ensure_branch_writable_with("sync", &live_branch)?;
        let sync_lease = self.begin_active_sync()?;
        let start = Instant::now();

        on_progress(0, 0, "scanning files");
        let phase_start = Instant::now();
        let current_files = self.scan_files();
        on_verbose(&format!(
            "scanned {} files in {:.1}s",
            current_files.len(),
            phase_start.elapsed().as_secs_f64()
        ));

        // Stat all files in parallel to get (mtime, size) — ~11ms for 20k files
        on_progress(0, 0, "checking file timestamps");
        let phase_start = Instant::now();
        let project_root = &self.project_root;
        let file_stats: Vec<(String, i64, u64)> = current_files
            .par_iter()
            .filter_map(|path| {
                let abs_path = project_root.join(path);
                let (mtime, size) = sync::file_stat(&abs_path)?;
                Some((path.clone(), mtime, size))
            })
            .collect();
        on_verbose(&format!(
            "stat-checked {} files in {:.1}s",
            file_stats.len(),
            phase_start.elapsed().as_secs_f64()
        ));

        // Load all DB file records into a map for O(1) lookups
        let db_files = self.db.get_all_files().await?;
        let db_map: HashMap<String, FileRecord> =
            db_files.into_iter().map(|f| (f.path.clone(), f)).collect();
        // A sync that starts from an empty DB builds the whole index, so the
        // persisted ref set is complete afterwards — used to skip the self-heal
        // re-extraction on this first sync (e.g. `init()` + `sync()` without a
        // prior `index_all`).
        let built_from_empty = db_map.is_empty();

        // Partition files by comparing (mtime, size) against stored values
        let mut new_files: Vec<String> = Vec::new();
        let mut stat_changed: Vec<String> = Vec::new();
        let mut current_set: std::collections::HashSet<&str> =
            std::collections::HashSet::with_capacity(file_stats.len());
        let mut stat_map: HashMap<String, (i64, u64)> = HashMap::with_capacity(file_stats.len());

        for (path, mtime, size) in &file_stats {
            current_set.insert(path.as_str());
            stat_map.insert(path.clone(), (*mtime, *size));
            match db_map.get(path) {
                None => new_files.push(path.clone()),
                Some(record) => {
                    if record.modified_at != *mtime || record.size != *size {
                        stat_changed.push(path.clone());
                    }
                }
            }
        }

        // Detect removed files from the same DB map
        let removed: Vec<String> = db_map
            .keys()
            .filter(|path| !current_set.contains(path.as_str()))
            .cloned()
            .collect();

        on_verbose(&format!(
            "changes: {} new, {} stat-changed, {} removed, {} unchanged",
            new_files.len(),
            stat_changed.len(),
            removed.len(),
            file_stats.len() - new_files.len() - stat_changed.len()
        ));

        // Read + hash only files with changed stats or new files
        on_progress(0, 0, "hashing changed files");
        let phase_start = Instant::now();
        let needs_read: Vec<&String> = new_files.iter().chain(stat_changed.iter()).collect();
        let hash_results: Vec<_> = needs_read
            .par_iter()
            .map(|path| {
                let abs_path = project_root.join(path.as_str());
                match sync::read_source_file(&abs_path) {
                    Ok(source) => Ok(((*path).clone(), sync::content_hash(&source))),
                    Err(e) => Err(((*path).clone(), e.to_string())),
                }
            })
            .collect();

        let mut skipped: Vec<(String, String)> = Vec::new();
        let mut hash_map: HashMap<String, String> = HashMap::new();
        for result in hash_results {
            match result {
                Ok((path, hash)) => {
                    hash_map.insert(path, hash);
                }
                Err((path, reason)) => {
                    skipped.push((path, reason));
                }
            }
        }
        on_verbose(&format!(
            "hashed {} files in {:.1}s ({} read errors)",
            hash_map.len(),
            phase_start.elapsed().as_secs_f64(),
            skipped.len()
        ));

        // Among stat_changed files, find those with actually different content
        on_progress(0, 0, "detecting changes");
        let mut stale: Vec<String> = Vec::new();
        let mut mtime_only_changed: Vec<String> = Vec::new();
        for path in &stat_changed {
            if let Some(new_hash) = hash_map.get(path)
                && let Some(record) = db_map.get(path)
            {
                if record.content_hash == *new_hash {
                    // mtime changed but content identical (e.g. touch) —
                    // update stored mtime so we skip it next time
                    mtime_only_changed.push(path.clone());
                } else {
                    stale.push(path.clone());
                }
            }
        }
        on_verbose(&format!(
            "content check: {} modified, {} mtime-only",
            stale.len(),
            mtime_only_changed.len()
        ));

        // Extract before entering the writer lane; no database state changes
        // are needed to parse the stale/new files.
        let to_index: Vec<String> = stale.iter().chain(new_files.iter()).cloned().collect();
        let registry = &self.registry;
        let phase_start = Instant::now();
        if checkpoint_writes {
            for path in &mtime_only_changed {
                if let (Some(record), Some(&(mtime, size))) = (db_map.get(path), stat_map.get(path))
                {
                    self.db
                        .upsert_file(&FileRecord {
                            modified_at: mtime,
                            size,
                            ..record.clone()
                        })
                        .await?;
                }
            }
            for path in &removed {
                on_progress(0, 0, &format!("removing {path}"));
                self.db.delete_file(path).await?;
            }

            let mut indexed = 0_usize;
            for paths in to_index.chunks(BRANCH_SYNC_EXTRACTION_PAGE_FILES) {
                let (extractions, batch_skipped) =
                    extract_files_isolated(project_root, registry, paths.to_vec());
                skipped.extend(batch_skipped);
                let mut batch_edges = Vec::new();
                for (file_path, result, hash, size, mtime) in &extractions {
                    indexed += 1;
                    on_progress(indexed, to_index.len(), file_path);
                    self.db.delete_nodes_by_file(file_path).await?;
                    for page in result.nodes.chunks(BRANCH_SYNC_WRITE_PAGE_ROWS) {
                        self.db.insert_nodes(page).await?;
                    }
                    self.db
                        .insert_unresolved_refs(&result.unresolved_refs)
                        .await?;
                    self.db
                        .upsert_file(&FileRecord {
                            path: file_path.clone(),
                            content_hash: hash.clone(),
                            size: *size,
                            modified_at: *mtime,
                            indexed_at: current_timestamp(),
                            node_count: result.nodes.len() as u32,
                        })
                        .await?;
                    batch_edges.extend(result.edges.iter().cloned());
                }
                for page in batch_edges.chunks(BRANCH_SYNC_WRITE_PAGE_ROWS) {
                    self.db.insert_edges(page).await?;
                }
                // Checkpointed daemon sync persists unresolved refs above but
                // deliberately avoids the global resolver, which loads every
                // graph node. Explicit/non-checkpointed sync retains the full
                // cross-file resolution contract.
            }
            if built_from_empty {
                self.db
                    .set_metadata(
                        UNRESOLVED_REFS_PERSISTED_KEY,
                        UNRESOLVED_REFS_PERSISTED_VALUE,
                    )
                    .await?;
            }
            let duration_ms = start.elapsed().as_millis() as u64;
            self.db
                .set_metadata("last_sync_at", &current_timestamp().to_string())
                .await?;
            self.stamp_last_synced_commit().await;
            self.touch_branch_meta_synced(&live_branch);
            self.db
                .set_metadata("last_sync_duration_ms", &duration_ms.to_string())
                .await?;
            self.db.checkpoint().await?;
            sync_lease.commit()?;
            let _ = self.db.release_connection_memory().await;
            release_process_allocator_memory();
            return Ok(SyncResult {
                files_added: new_files.len(),
                files_modified: stale.len(),
                files_removed: removed.len(),
                duration_ms,
                added_paths: new_files,
                modified_paths: stale,
                skipped_paths: skipped,
                removed_paths: removed,
            });
        }
        let (sync_extractions, sync_skipped): (Vec<_>, Vec<_>) =
            extract_files_isolated(project_root, registry, to_index.clone());
        // Surface extractor timeouts/crashes in `SyncResult.skipped_paths`
        // so the user can see them in `tracedecay sync --doctor`.
        skipped.extend(sync_skipped);

        let total = sync_extractions.len();
        let mut total_nodes = 0;
        let mut queued_edges = Vec::new();
        let mut file_records = Vec::with_capacity(total);
        for (idx, (file_path, result, hash, size, mtime)) in sync_extractions.iter().enumerate() {
            on_progress(idx + 1, total, file_path);
            total_nodes += result.nodes.len();
            queued_edges.extend(result.edges.iter().cloned());
            file_records.push(FileRecord {
                path: file_path.clone(),
                content_hash: hash.clone(),
                size: *size,
                modified_at: *mtime,
                indexed_at: current_timestamp(),
                node_count: result.nodes.len() as u32,
            });
        }
        let total_edges = queued_edges.len();

        // Only the non-checkpointed path reaches here: the `checkpoint_writes`
        // block above returns unconditionally.
        let transaction = self.db.begin_write_transaction("sync index").await?;
        for path in &mtime_only_changed {
            if let (Some(record), Some(&(mtime, size))) = (db_map.get(path), stat_map.get(path)) {
                self.db
                    .upsert_file_unguarded(
                        &transaction,
                        &FileRecord {
                            modified_at: mtime,
                            size,
                            ..record.clone()
                        },
                    )
                    .await?;
            }
        }
        for path in &removed {
            on_progress(0, 0, &format!("removing {path}"));
            self.db.delete_file_unguarded(&transaction, path).await?;
        }
        for ((file_path, result, _, _, _), file_record) in
            sync_extractions.iter().zip(&file_records)
        {
            self.db
                .delete_nodes_by_file_unguarded(&transaction, file_path)
                .await?;
            self.db
                .insert_nodes_unguarded(&transaction, &result.nodes)
                .await?;
            self.db
                .insert_unresolved_refs_unguarded(&transaction, &result.unresolved_refs)
                .await?;
            self.db
                .upsert_file_unguarded(&transaction, file_record)
                .await?;
        }
        self.db
            .insert_edges_unguarded(&transaction, &queued_edges)
            .await?;
        if built_from_empty {
            self.db
                .set_metadata_unguarded(
                    &transaction,
                    UNRESOLVED_REFS_PERSISTED_KEY,
                    UNRESOLVED_REFS_PERSISTED_VALUE,
                )
                .await?;
        }
        transaction.commit().await?;

        if !to_index.is_empty() {
            on_verbose(&format!(
                "indexed {} files ({} nodes, {} edges) in {:.1}s",
                to_index.len(),
                total_nodes,
                total_edges,
                phase_start.elapsed().as_secs_f64()
            ));
        }

        // Resolve references (call edges, uses, etc.) across all files.
        // This must run after all files are indexed so cross-file references
        // can find their targets.
        if !to_index.is_empty() {
            on_progress(0, 0, "resolving references");
            let phase_start = Instant::now();
            let (short, keys) = reindexed_symbol_scope(&sync_extractions);
            self.reresolve_after_reindex(&to_index, &short, &keys)
                .await?;
            on_verbose(&format!(
                "resolved references in {:.1}s",
                phase_start.elapsed().as_secs_f64()
            ));
        } else if self.heal_unresolved_refs_if_needed().await? {
            // Eager heal: a clean repo (zero changed files) indexed before
            // refs were persisted would otherwise never reach the heal —
            // it only ran on the re-index path, so its dropped cross-file
            // edges stayed missing until some file happened to change. The
            // marker check drives the heal directly; the at-most-once
            // guarantee lives in the marker stamped inside the heal, and a
            // healed (or `built_from_empty`-stamped) index skips this with a
            // single metadata read.
            on_progress(0, 0, "resolving references");
            let phase_start = Instant::now();
            self.resolve_all_unresolved_refs().await?;
            on_verbose(&format!(
                "healed and resolved references in {:.1}s",
                phase_start.elapsed().as_secs_f64()
            ));
        }

        let duration_ms = start.elapsed().as_millis() as u64;
        self.db
            .set_metadata("last_sync_at", &current_timestamp().to_string())
            .await?;
        // Stamp HEAD so the watcher can diff-scope future syncs (best-effort).
        self.stamp_last_synced_commit().await;
        self.touch_branch_meta_synced(&live_branch);
        self.db
            .set_metadata("last_sync_duration_ms", &duration_ms.to_string())
            .await?;

        self.db.checkpoint().await?;
        sync_lease.commit()?;
        let _ = self.db.release_connection_memory().await;
        release_process_allocator_memory();
        Ok(SyncResult {
            files_added: new_files.len(),
            files_modified: stale.len(),
            files_removed: removed.len(),
            duration_ms,
            added_paths: new_files,
            modified_paths: stale,
            skipped_paths: skipped,
            removed_paths: removed,
        })
    }
}

// ---------------------------------------------------------------------------
// Staleness detection
// ---------------------------------------------------------------------------

impl TraceDecay {
    /// Check whether the given files need (re-/un-)indexing to bring the DB
    /// into agreement with the filesystem.
    ///
    /// A file is reported stale when any of:
    /// - it is in the DB and has been modified on disk since `indexed_at`,
    /// - it is in the DB but no longer exists on disk (deletion — DB needs cleanup),
    /// - it exists on disk but has no DB record (new file — needs indexing).
    ///
    /// A file that exists in neither the DB nor on disk is out of scope and
    /// is silently dropped.
    pub async fn check_file_staleness(&self, file_paths: &[String]) -> Vec<String> {
        let mut stale = Vec::new();
        for path in file_paths {
            // Match the DB's canonical form (forward slashes). Without this,
            // a caller passing `src\foo.py` on Windows misses the row stored
            // under `src/foo.py` and the file gets treated as "new" — a
            // subsequent sync would insert a *second* row alongside the
            // original, which is #87.
            let normalized = normalize_rel_path(path);
            let abs_path = self.project_root.join(&normalized);
            let file_exists = abs_path.exists();
            match self.db.get_file(&normalized).await {
                Ok(Some(record)) => {
                    if !file_exists {
                        // Indexed but deleted — DB needs cleanup.
                        stale.push(normalized);
                    } else if let Ok(metadata) = std::fs::metadata(&abs_path)
                        && let Ok(mtime) = metadata.modified()
                    {
                        let mtime_secs = mtime
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs() as i64;
                        if mtime_secs > record.indexed_at {
                            stale.push(normalized);
                        }
                    }
                }
                _ => {
                    // Not in the DB. If it exists on disk, it's new and needs indexing.
                    if file_exists {
                        stale.push(normalized);
                    }
                }
            }
        }
        stale
    }

    /// Returns every file whose on-disk mtime is newer than its indexed
    /// timestamp, plus on-disk files the DB doesn't know about yet, plus
    /// DB-known files that no longer exist on disk (so a follow-up sync
    /// can prune them).
    ///
    /// Walks the project tree with the same gitignore-aware logic used by
    /// `sync()`, then compares against a single batched DB read of the
    /// `files` table — no per-file SQL round trips. This is the
    /// notification-free replacement for the `notify`-based watcher
    /// removed in v6.x (see #80): the MCP server calls it on a 30 s
    /// cooldown to keep the index fresh without burning CPU/memory on
    /// kernel event streams.
    pub async fn find_stale_files(&self) -> Vec<String> {
        let on_disk = self.scan_files();
        // DB read failed → be conservative and treat every on-disk file as
        // stale rather than silently dropping the check.
        let Ok(indexed) = self.get_all_files().await else {
            return on_disk;
        };

        let indexed_map: HashMap<&str, i64> = indexed
            .iter()
            .map(|f| (f.path.as_str(), f.indexed_at))
            .collect();
        let on_disk_set: HashSet<&str> = on_disk.iter().map(String::as_str).collect();

        let mut stale: Vec<String> = Vec::new();

        for rel in &on_disk {
            let abs = self.project_root.join(rel);
            let mtime_secs = std::fs::metadata(&abs)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs() as i64);
            match indexed_map.get(rel.as_str()) {
                Some(&indexed_at) if mtime_secs <= indexed_at => {}
                _ => stale.push(rel.clone()),
            }
        }

        for indexed_path in indexed_map.keys() {
            if !on_disk_set.contains(*indexed_path) {
                stale.push((*indexed_path).to_string());
            }
        }

        stale.sort();
        stale.dedup();
        stale
    }

    /// Returns the most recent `indexed_at` timestamp across all indexed files.
    pub async fn last_index_time(&self) -> Result<i64> {
        self.db.last_index_time().await
    }

    /// Returns the timestamp of the most recent successful sync.
    ///
    /// Prefers the `last_sync_at` metadata key, which advances on every sync
    /// invocation regardless of whether any files actually changed. Falls
    /// back to `last_index_time` (the max file `indexed_at`) only if the
    /// metadata key is missing or unreadable — that fallback gives the wrong
    /// answer on quiet repos because `indexed_at` is per-file and only moves
    /// when a file is reindexed, which is exactly the bug #86 was reporting.
    pub async fn last_sync_timestamp(&self) -> i64 {
        if let Ok(Some(raw)) = self.db.get_metadata("last_sync_at").await
            && let Ok(t) = raw.parse::<i64>()
        {
            return t;
        }
        self.db.last_index_time().await.unwrap_or(0)
    }

    /// Count git commits newer than the given UNIX timestamp.
    /// Returns 0 if git is unavailable or the directory is not a git repository.
    pub fn git_commits_since(&self, since_timestamp: i64) -> usize {
        let Ok(repo) = gix::open(&self.project_root) else {
            return 0;
        };
        let Ok(head) = repo.head_commit() else {
            return 0;
        };
        let sorting = gix::revision::walk::Sorting::ByCommitTimeCutoff {
            order: gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
            seconds: since_timestamp,
        };
        let Ok(walk) = head.ancestors().sorting(sorting).all() else {
            return 0;
        };
        walk.filter_map(std::result::Result::ok).count()
    }

    /// Stamps the current git HEAD commit id into the `last_synced_commit`
    /// metadata key so the git-metadata watcher can diff-scope future syncs
    /// (see [`stale_files_since_commit`](Self::stale_files_since_commit)).
    ///
    /// Called on the success path of every sync alongside `last_sync_at`. On
    /// any gix error (not a repo, detached/unborn HEAD, unreadable object)
    /// this is a silent no-op — a missing/stale stamp only forces the watcher
    /// to fall back to a full tree walk, never a failed sync.
    /// Best-effort branch-meta freshness stamp on every successful sync, so
    /// `branch_list` reflects real sync recency rather than branch-add time
    /// only. No-op when the active branch cannot be resolved (detached HEAD)
    /// or the branch is untracked.
    fn touch_branch_meta_synced(&self, live_branch: &crate::branch::BranchMemo) {
        if let Some(branch) = live_branch.resolve_for(&self.project_root) {
            crate::branch_meta::update_synced_timestamp(&self.store_layout.data_root, &branch);
        }
    }

    async fn stamp_last_synced_commit(&self) {
        // Scope the gix values so they drop before the `.await`:
        // `gix::Repository`/`Commit` are `!Send`, and holding them across the
        // await would make every sync future `!Send`, breaking `tokio::spawn`
        // of anything that syncs (sync-on-read, scheduler, git watcher).
        let hex_oid = {
            let Ok(repo) = gix::open(&self.project_root) else {
                return;
            };
            let Ok(head) = repo.head_commit() else {
                return;
            };
            head.id().to_hex().to_string()
        };
        let _ = self.db.set_metadata("last_synced_commit", &hex_oid).await;
    }

    /// Returns the git HEAD commit id recorded at the last successful sync,
    /// or `None` if it was never stamped (e.g. index predates this feature,
    /// or HEAD could not be resolved at sync time).
    pub async fn last_synced_commit(&self) -> Option<String> {
        self.db
            .get_metadata("last_synced_commit")
            .await
            .ok()
            .flatten()
    }

    /// Computes the repo-relative paths changed between `base_commit` and the
    /// current git HEAD via a gix tree diff (both directions, including
    /// deletions), for diff-scoped incremental syncs by the watcher.
    ///
    /// Returns:
    /// - `Some(paths)` (forward-slash normalized) on a successful bounded diff;
    /// - `None` when `base_commit` is missing/unreachable (force-push, gc) — the
    ///   caller must fall back to a full [`find_stale_files`](Self::find_stale_files)
    ///   tree walk / [`sync`](Self::sync);
    /// - `None` when the changed-file count exceeds `escalation_limit`, again
    ///   signalling a full sync rather than an oversized targeted pass.
    ///
    /// Does not touch the DB; the watcher wires it into `sync_if_stale_silent`.
    pub fn stale_files_since_commit(
        &self,
        base_commit: &str,
        escalation_limit: usize,
    ) -> Option<Vec<String>> {
        let repo = gix::open(&self.project_root).ok()?;

        let base_tree = repo
            .rev_parse_single(base_commit)
            .ok()?
            .object()
            .ok()?
            .peel_to_tree()
            .ok()?;

        let head_tree = repo.head_commit().ok()?.tree().ok()?;

        let mut changed: HashSet<String> = HashSet::new();
        base_tree
            .changes()
            .ok()?
            .for_each_to_obtain_tree(&head_tree, |change| {
                use gix::object::tree::diff::Change;
                {
                    let mut push = |loc: &gix::bstr::BStr, mode: &gix::object::tree::EntryMode| {
                        if !mode.is_tree() {
                            let path = loc.to_string();
                            if !is_tracedecay_state_path(&path) {
                                changed.insert(path);
                            }
                        }
                    };
                    match &change {
                        Change::Addition {
                            location,
                            entry_mode,
                            ..
                        }
                        | Change::Modification {
                            location,
                            entry_mode,
                            ..
                        }
                        | Change::Deletion {
                            location,
                            entry_mode,
                            ..
                        } => push(location, entry_mode),
                        Change::Rewrite {
                            source_location,
                            source_entry_mode,
                            location,
                            entry_mode,
                            ..
                        } => {
                            push(source_location, source_entry_mode);
                            push(location, entry_mode);
                        }
                    }
                }
                // Stop early once the change set is clearly a full-sync case;
                // the caller escalates on `None` anyway.
                if changed.len() > escalation_limit {
                    Ok::<_, std::convert::Infallible>(std::ops::ControlFlow::Break(()))
                } else {
                    Ok(std::ops::ControlFlow::Continue(()))
                }
            })
            .ok()?;

        // Too many changes → let the caller do a full sync instead.
        if changed.len() > escalation_limit {
            return None;
        }

        let paths: Vec<String> = changed.into_iter().collect();
        Some(normalize_rel_paths(&paths))
    }
}
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod freshness_tests;
#[cfg(test)]
mod path_normalization_tests;

#[cfg(test)]
mod worker_pool_tests {
    use super::{MAX_EXTRACTION_WORKERS, bounded_extraction_workers};

    #[test]
    fn extraction_worker_count_bounds_parallel_allocators() {
        assert_eq!(
            bounded_extraction_workers(96, 2_467),
            MAX_EXTRACTION_WORKERS
        );
        assert_eq!(bounded_extraction_workers(96, 3), 3);
        assert_eq!(bounded_extraction_workers(0, 0), 1);
    }
}

#[cfg(test)]
mod migration_reindex_tests {
    use super::{
        MigrationReindexAvailabilityV1, MigrationReindexCheckpointBatchV1,
        MigrationReindexCheckpointEntryV1, MigrationReindexStatusV1,
        should_schedule_migration_reindex,
    };

    #[test]
    fn pending_and_interrupted_migration_generations_resume() {
        let pending = MigrationReindexStatusV1::Indexing {
            schema_version: 25,
            completed_files: 0,
            total_files: None,
            availability: MigrationReindexAvailabilityV1::Stale,
        };
        let interrupted = MigrationReindexStatusV1::Failed {
            schema_version: 25,
            availability: MigrationReindexAvailabilityV1::Unavailable,
            retryable: true,
            reason: "worker terminated".to_owned(),
        };

        assert!(should_schedule_migration_reindex(&pending));
        assert!(should_schedule_migration_reindex(&interrupted));
    }

    #[test]
    fn current_migration_generation_does_not_rebuild() {
        let current = MigrationReindexStatusV1::Current {
            schema_version: 25,
            completed_at: 42,
        };
        let terminal = MigrationReindexStatusV1::Failed {
            schema_version: 25,
            availability: MigrationReindexAvailabilityV1::Unavailable,
            retryable: false,
            reason: "manual intervention required".to_owned(),
        };

        assert!(!should_schedule_migration_reindex(&current));
        assert!(!should_schedule_migration_reindex(&terminal));
    }

    #[test]
    fn migration_state_is_typed_and_restart_safe() {
        let state = MigrationReindexStatusV1::Indexing {
            schema_version: 25,
            completed_files: 17,
            total_files: Some(41),
            availability: MigrationReindexAvailabilityV1::Stale,
        };

        let encoded = serde_json::to_string(&state).expect("encode migration state");
        let restored: MigrationReindexStatusV1 =
            serde_json::from_str(&encoded).expect("restore migration state");

        assert_eq!(restored, state);
        assert!(encoded.contains("\"state\":\"indexing\""));
        assert!(encoded.contains("\"availability\":\"stale\""));
    }

    #[test]
    fn migration_checkpoint_retains_completed_extraction_across_restart() {
        let batch = MigrationReindexCheckpointBatchV1 {
            schema_version: 25,
            entries: vec![MigrationReindexCheckpointEntryV1 {
                file_path: "src/lib.rs".to_owned(),
                result: crate::types::ExtractionResult {
                    nodes: Vec::new(),
                    edges: Vec::new(),
                    unresolved_refs: Vec::new(),
                    errors: Vec::new(),
                    duration_ms: 7,
                },
                content_hash: "content-v1".to_owned(),
                size: 19,
                modified_at: 42,
            }],
        };

        let encoded = serde_json::to_vec(&batch).expect("encode checkpoint");
        let restored: MigrationReindexCheckpointBatchV1 =
            serde_json::from_slice(&encoded).expect("restore checkpoint");

        assert_eq!(restored.schema_version, 25);
        assert_eq!(restored.entries.len(), 1);
        assert_eq!(restored.entries[0].file_path, "src/lib.rs");
        assert_eq!(restored.entries[0].result.duration_ms, 7);
    }
}
