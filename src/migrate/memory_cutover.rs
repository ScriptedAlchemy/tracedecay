//! Explicit offline union of branch-local legacy memory into project memory.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_domain::{FactOwnerV1, ProjectId, RefId, SourceStoreId};
use tracedecay_store::CompatibilityLegacyMemoryCutoverProgressV1;

use crate::branch_meta;
use crate::db::engine::QueryExecutor;
use crate::errors::{Result, TraceDecayError};
use crate::storage;
use crate::tracedecay::{TraceDecay, TraceDecayOpenOptions};

const LEGACY_SOURCE_STORE: &str = "legacy-memory-v1";
const RECEIPT_FILENAME: &str = "memory-branch-cutover.json";
const MAX_CUTOVER_PASSES: usize = 100_000;
const MIN_SUPPORTED_SOURCE_SCHEMA: i64 = 15;

#[derive(Clone, Debug)]
pub struct MemoryCutoverOptions {
    pub project_root: PathBuf,
    pub profile_root: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemoryCutoverSource {
    pub path: PathBuf,
    pub user_version: i64,
    pub fact_count: u64,
    pub feedback_count: u64,
    pub oplog_count: u64,
    pub unmapped_memory_v2_facts: u64,
    pub generation: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemoryCutoverReport {
    pub project_id: String,
    pub project_graph: PathBuf,
    pub sources: Vec<MemoryCutoverSource>,
    pub confirmation_token: String,
    pub applied: bool,
    pub cutover_passes: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BranchMemoryCutoverReceipt {
    version: u32,
    project_id: String,
    completed_at: i64,
    sources: Vec<BranchMemoryCutoverReceiptSource>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BranchMemoryCutoverReceiptSource {
    relative_path: PathBuf,
    generation: String,
}

pub async fn plan(options: &MemoryCutoverOptions) -> Result<MemoryCutoverReport> {
    let resolved = resolve(options)?;
    let scratch = resolved.data_root.join("scratch").join("memory-cutover");
    storage::PrivateStoreIo::create_dir_all(&scratch)?;
    let mut sources = Vec::new();
    for path in branch_database_paths(&resolved.data_root)? {
        let snapshot = crate::sqlite_read_snapshot::open_in(&path, &scratch)
            .await
            .map_err(|error| migration_error(format!("snapshot '{}': {error}", path.display())))?;
        let user_version = scalar_i64(snapshot.connection(), "PRAGMA user_version").await?;
        if user_version < MIN_SUPPORTED_SOURCE_SCHEMA {
            return Err(migration_error(format!(
                "branch memory source '{}' uses unsupported schema v{user_version}; \
                 v{MIN_SUPPORTED_SOURCE_SCHEMA} or newer is required",
                path.display()
            )));
        }
        let fact_count = table_count(snapshot.connection(), "memory_facts").await?;
        let feedback_count = table_count(snapshot.connection(), "memory_feedback_events").await?;
        let oplog_count = table_count(snapshot.connection(), "memory_oplog").await?;
        let unmapped_memory_v2_facts =
            if table_exists(snapshot.connection(), "memory_v2_facts").await? {
                scalar_u64(
                    snapshot.connection(),
                    "SELECT COUNT(*) FROM memory_v2_facts AS fact
                     WHERE NOT EXISTS(
                         SELECT 1 FROM memory_v2_legacy_map AS mapping
                         WHERE mapping.fact_id = fact.fact_id
                           AND mapping.owner_kind = fact.owner_kind
                           AND mapping.project_id = fact.project_id
                     )",
                )
                .await?
            } else {
                0
            };
        snapshot
            .validate_source()
            .map_err(|error| migration_error(error.to_string()))?;
        sources.push(MemoryCutoverSource {
            generation: source_generation(&path)?,
            path,
            user_version,
            fact_count,
            feedback_count,
            oplog_count,
            unmapped_memory_v2_facts,
        });
    }
    let confirmation_token =
        confirmation_token(&resolved.project_id, &resolved.graph_db_path, &sources);
    Ok(MemoryCutoverReport {
        project_id: resolved.project_id,
        project_graph: resolved.graph_db_path,
        sources,
        confirmation_token,
        applied: false,
        cutover_passes: 0,
    })
}

pub async fn apply(
    options: &MemoryCutoverOptions,
    expected_confirmation_token: &str,
) -> Result<MemoryCutoverReport> {
    let planned = plan(options).await?;
    if planned.confirmation_token != expected_confirmation_token {
        return Err(migration_error(
            "memory cutover confirmation token does not match the current branch-store generation",
        ));
    }
    if let Some(source) = planned
        .sources
        .iter()
        .find(|source| source.unmapped_memory_v2_facts > 0)
    {
        return Err(migration_error(format!(
            "branch memory source '{}' has {} Memory V2 fact(s) without a legacy mirror; \
             refusing a lossy cutover",
            source.path.display(),
            source.unmapped_memory_v2_facts
        )));
    }

    let resolved = resolve(options)?;
    let lifecycle = crate::lifecycle_lease::acquire_exclusive_for_profile(
        &resolved.profile_root,
        "project-wide branch memory cutover",
    )?;
    let _database_scope = crate::db::enter_maintenance_database_scope(
        &lifecycle,
        &resolved.profile_root,
        "project-wide branch memory cutover",
    )?;
    let graph = TraceDecay::open_branch_with_exclusive_maintenance(
        &resolved.project_root,
        &resolved.default_branch,
        TraceDecayOpenOptions {
            profile_root: Some(resolved.profile_root.clone()),
            global_db_path: None,
        },
        &lifecycle,
    )
    .await?;
    let target = graph.open_project_store_db().await?;
    let scratch = resolved.data_root.join("scratch").join("memory-cutover");
    storage::PrivateStoreIo::create_dir_all(&scratch)?;
    for source in &planned.sources {
        if source_generation(&source.path)? != source.generation {
            return Err(migration_error(format!(
                "branch memory source '{}' changed after planning",
                source.path.display()
            )));
        }
        let snapshot = crate::sqlite_read_snapshot::open_in(&source.path, &scratch)
            .await
            .map_err(|error| {
                migration_error(format!("snapshot '{}': {error}", source.path.display()))
            })?;
        crate::migrate::consolidate::sqlite::merge_branch_legacy_memory_snapshot(
            &target, &snapshot,
        )
        .await?;
    }
    crate::migrate::consolidate::sqlite::rebuild_branch_cutover_memory_banks(&target).await?;

    let owner = FactOwnerV1::Project {
        project_id: ProjectId::new(resolved.project_id.clone())
            .map_err(|error| migration_error(error.to_string()))?,
    };
    let source_store_id = SourceStoreId::new(LEGACY_SOURCE_STORE.to_owned())
        .map_err(|error| migration_error(error.to_string()))?;
    target
        .reopen_memory_v2_cutover_for_legacy_union(&owner, &source_store_id)
        .await?;

    let mut cutover_passes = 0;
    loop {
        cutover_passes += 1;
        if cutover_passes > MAX_CUTOVER_PASSES {
            return Err(migration_error(
                "memory cutover exceeded its bounded pass limit",
            ));
        }
        if graph.advance_project_memory_cutover_once().await?
            == CompatibilityLegacyMemoryCutoverProgressV1::Complete
        {
            break;
        }
    }
    write_cutover_receipt(&resolved, &planned.sources)?;

    Ok(MemoryCutoverReport {
        applied: true,
        cutover_passes,
        ..planned
    })
}

/// Fails closed unless every selected branch family exactly matches a source
/// generation covered by a completed project-wide memory cutover.
pub fn verify_branch_removal_receipts(
    data_root: &Path,
    original_paths: &[PathBuf],
    validation_paths: &[PathBuf],
) -> Result<()> {
    let receipt_path = data_root.join(RECEIPT_FILENAME);
    let receipt = match fs::read(&receipt_path) {
        Ok(bytes) => {
            let receipt: BranchMemoryCutoverReceipt =
                serde_json::from_slice(&bytes).map_err(|error| {
                    migration_error(format!(
                        "project-memory cutover receipt '{}' is invalid: {error}",
                        receipt_path.display()
                    ))
                })?;
            if receipt.version != 1 {
                return Err(migration_error(
                    "unsupported project-memory cutover receipt",
                ));
            }
            Some(receipt)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(migration_error(format!(
                "cannot read project-memory cutover receipt '{}': {error}",
                receipt_path.display()
            )));
        }
    };
    for original in original_paths {
        let relative = original.strip_prefix(data_root).map_err(|_| {
            migration_error(format!(
                "branch database '{}' escapes its project store",
                original.display()
            ))
        })?;
        let candidate = if original.exists() {
            original.clone()
        } else {
            let original_name = original
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| migration_error("branch database filename is not UTF-8"))?;
            validation_paths
                .iter()
                .find(|path| {
                    path.exists()
                        && path.parent() == original.parent()
                        && path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| {
                                name.starts_with(&format!(".{original_name}.branch-delete-"))
                                    && name.ends_with(".quarantine")
                            })
                })
                .cloned()
                .ok_or_else(|| {
                    migration_error(format!(
                        "cannot locate quarantined family for '{}'",
                        original.display()
                    ))
                })?
        };
        let expected = receipt.as_ref().and_then(|receipt| {
            receipt
                .sources
                .iter()
                .find(|source| source.relative_path == relative)
        });
        let Some(expected) = expected else {
            if branch_has_no_durable_memory(&candidate) {
                continue;
            }
            return Err(migration_error(format!(
                "branch database '{}' has durable memory but no completed project-memory cutover receipt",
                original.display()
            )));
        };
        let actual = source_generation(&candidate)?;
        if actual != expected.generation {
            return Err(migration_error(format!(
                "branch database '{}' changed after project-memory cutover; deletion refused",
                original.display()
            )));
        }
    }
    Ok(())
}

fn branch_has_no_durable_memory(path: &Path) -> bool {
    let wal = PathBuf::from(format!("{}-wal", path.display()));
    if fs::metadata(&wal).is_ok_and(|metadata| metadata.len() > 0) {
        return false;
    }
    let Ok(connection) = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return false;
    };
    for table in [
        "memory_facts",
        "memory_feedback_events",
        "memory_oplog",
        "memory_v2_facts",
    ] {
        let exists = connection
            .query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1
                 )",
                [table],
                |row| row.get::<_, bool>(0),
            )
            .unwrap_or(true);
        if !exists {
            continue;
        }
        let sql = format!("SELECT EXISTS(SELECT 1 FROM \"{table}\" LIMIT 1)");
        if connection
            .query_row(&sql, [], |row| row.get::<_, bool>(0))
            .unwrap_or(true)
        {
            return false;
        }
    }
    true
}

struct ResolvedMemoryCutover {
    project_root: PathBuf,
    profile_root: PathBuf,
    data_root: PathBuf,
    graph_db_path: PathBuf,
    project_id: String,
    default_branch: String,
}

fn resolve(options: &MemoryCutoverOptions) -> Result<ResolvedMemoryCutover> {
    let project_root = options
        .project_root
        .canonicalize()
        .map_err(|error| migration_error(format!("resolve project root: {error}")))?;
    let profile_root = options
        .profile_root
        .canonicalize()
        .map_err(|error| migration_error(format!("resolve profile root: {error}")))?;
    let marker = storage::read_enrollment_marker(&project_root)?.ok_or_else(|| {
        migration_error(format!(
            "project '{}' is not enrolled in profile-sharded storage",
            project_root.display()
        ))
    })?;
    let layout = storage::profile_sharded_layout(&project_root, &profile_root, &marker)?;
    let branch_meta = branch_meta::load_branch_meta(&layout.data_root)
        .ok_or_else(|| migration_error("branch metadata is required for memory cutover"))?;
    Ok(ResolvedMemoryCutover {
        project_root,
        profile_root,
        data_root: layout.data_root,
        graph_db_path: layout.graph_db_path,
        project_id: marker.project_id,
        default_branch: branch_meta.default_branch,
    })
}

fn branch_database_paths(data_root: &Path) -> Result<Vec<PathBuf>> {
    let branches = data_root.join("branches");
    let mut paths = Vec::new();
    for entry in fs::read_dir(&branches).map_err(|error| {
        migration_error(format!(
            "read branch database directory '{}': {error}",
            branches.display()
        ))
    })? {
        let entry = entry.map_err(|error| migration_error(error.to_string()))?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) == Some("db") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn confirmation_token(
    project_id: &str,
    graph_db_path: &Path,
    sources: &[MemoryCutoverSource],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.project-memory-cutover.v1\0");
    hasher.update(project_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(graph_db_path.as_os_str().as_encoded_bytes());
    for source in sources {
        hasher.update(b"\0");
        hasher.update(source.path.as_os_str().as_encoded_bytes());
        hasher.update(b"\0");
        hasher.update(source.generation.as_bytes());
    }
    format!("confirm-memory-cutover-{:x}", hasher.finalize())
}

fn source_generation(path: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.sqlite-family-generation.v1\0");
    for suffix in ["", "-wal", "-shm"] {
        let member = PathBuf::from(format!("{}{suffix}", path.display()));
        hasher.update(suffix.as_bytes());
        match fs::metadata(&member) {
            Ok(metadata) => {
                hasher.update([1]);
                hasher.update(metadata.len().to_le_bytes());
                let modified = metadata
                    .modified()
                    .ok()
                    .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                    .unwrap_or_default();
                hasher.update(modified.as_secs().to_le_bytes());
                hasher.update(modified.subsec_nanos().to_le_bytes());
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    hasher.update(metadata.dev().to_le_bytes());
                    hasher.update(metadata.ino().to_le_bytes());
                }
                let mut file =
                    fs::File::open(&member).map_err(|error| migration_error(error.to_string()))?;
                let mut header = [0_u8; 128];
                let read = file
                    .read(&mut header)
                    .map_err(|error| migration_error(error.to_string()))?;
                hasher.update((read as u64).to_le_bytes());
                hasher.update(&header[..read]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => hasher.update([0]),
            Err(error) => return Err(migration_error(error.to_string())),
        }
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn write_cutover_receipt(
    resolved: &ResolvedMemoryCutover,
    sources: &[MemoryCutoverSource],
) -> Result<()> {
    let receipt = BranchMemoryCutoverReceipt {
        version: 1,
        project_id: resolved.project_id.clone(),
        completed_at: crate::tracedecay::current_timestamp(),
        sources: sources
            .iter()
            .map(|source| {
                let relative_path = source
                    .path
                    .strip_prefix(&resolved.data_root)
                    .map(Path::to_path_buf)
                    .map_err(|_| {
                        migration_error(format!(
                            "branch source '{}' escapes project store",
                            source.path.display()
                        ))
                    })?;
                Ok(BranchMemoryCutoverReceiptSource {
                    relative_path,
                    generation: source.generation.clone(),
                })
            })
            .collect::<Result<Vec<_>>>()?,
    };
    let path = resolved.data_root.join(RECEIPT_FILENAME);
    let temp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    let bytes =
        serde_json::to_vec_pretty(&receipt).map_err(|error| migration_error(error.to_string()))?;
    storage::PrivateStoreIo::write_file_atomically(&path, &temp, &bytes)
        .map_err(|error| migration_error(error.to_string()))
}

async fn table_count(connection: &impl QueryExecutor, table: &str) -> Result<u64> {
    if !table_exists(connection, table).await? {
        return Ok(0);
    }
    scalar_u64(connection, &format!("SELECT COUNT(*) FROM \"{table}\"")).await
}

async fn table_exists(connection: &impl QueryExecutor, table: &str) -> Result<bool> {
    let mut rows = connection
        .query(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            crate::db::engine::params![table],
        )
        .await
        .map_err(|error| migration_error(error.to_string()))?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| migration_error(error.to_string()))
}

async fn scalar_u64(connection: &impl QueryExecutor, sql: &str) -> Result<u64> {
    let value = scalar_i64(connection, sql).await?;
    u64::try_from(value).map_err(|error| migration_error(error.to_string()))
}

async fn scalar_i64(connection: &impl QueryExecutor, sql: &str) -> Result<i64> {
    let mut rows = connection
        .query(sql, ())
        .await
        .map_err(|error| migration_error(error.to_string()))?;
    rows.next()
        .await
        .map_err(|error| migration_error(error.to_string()))?
        .ok_or_else(|| migration_error("scalar query returned no row"))?
        .get(0)
        .map_err(|error| migration_error(error.to_string()))
}

fn migration_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Database {
        message: message.into(),
        operation: "project_memory_cutover".to_owned(),
    }
}
