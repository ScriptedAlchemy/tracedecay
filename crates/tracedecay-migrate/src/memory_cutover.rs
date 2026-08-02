//! Explicit offline union of branch-local legacy memory into project memory.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
#[cfg(any(test, feature = "test-transport"))]
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use tracedecay_domain::{FactOwnerV1, ProjectId, ProvenanceId, SourceStoreId};
use tracedecay_store::{
    CompatibilityLegacyMemoryCutoverCommandV1, CompatibilityLegacyMemoryCutoverProgressV1,
    FactCompatibilityStore, MEMORY_V2_OWNER_ARCHIVE_SCHEMA_V1, plan_memory_v2_owner_merge,
};

use tracedecay_runtime_core::branch_meta;
use tracedecay_runtime_core::db::engine::QueryExecutor;
use tracedecay_runtime_core::db::{
    MemoryV2ArchiveDatabase, export_memory_v2_owner_archive, list_memory_v2_archive_owners,
};
use tracedecay_runtime_core::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::storage;
use tracedecay_runtime_core::store::memory::DatabaseFactStore;

const LEGACY_SOURCE_STORE: &str = "legacy-memory-v1";
const RECEIPT_FILENAME: &str = "memory-branch-cutover.json";
const MAX_CUTOVER_PASSES: usize = 100_000;
const MIN_SUPPORTED_SOURCE_SCHEMA: i64 = 15;

#[cfg(any(test, feature = "test-transport"))]
static CUTOVER_FAULT: AtomicU8 = AtomicU8::new(0);

#[cfg(any(test, feature = "test-transport"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub enum CutoverFaultForTest {
    TargetDurabilityBarrier = 1,
    ReceiptDurability = 2,
    ReceiptAfterRename = 3,
}

#[cfg(any(test, feature = "test-transport"))]
#[doc(hidden)]
pub fn set_cutover_fault_for_test(fault: CutoverFaultForTest) {
    match fault {
        CutoverFaultForTest::TargetDurabilityBarrier => {
            CUTOVER_FAULT.store(fault as u8, Ordering::SeqCst);
        }
        CutoverFaultForTest::ReceiptDurability => {
            CUTOVER_FAULT.store(0, Ordering::SeqCst);
            storage::set_durable_atomic_write_fault_for_test(
                storage::DurableAtomicWriteFaultForTest::AfterTempSync,
            );
        }
        CutoverFaultForTest::ReceiptAfterRename => {
            CUTOVER_FAULT.store(0, Ordering::SeqCst);
            storage::set_durable_atomic_write_fault_for_test(
                storage::DurableAtomicWriteFaultForTest::AfterRename,
            );
        }
    }
}

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
    pub memory_v2_fact_count: u64,
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
    archive_schema: String,
    completed_at: i64,
    sources: Vec<BranchMemoryCutoverReceiptSource>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BranchMemoryCutoverReceiptSource {
    relative_path: PathBuf,
    generation: String,
    legacy_only: bool,
    archives: Vec<crate::consolidate::sqlite::MemoryV2ArchiveMergeProof>,
}

pub async fn plan(options: &MemoryCutoverOptions) -> Result<MemoryCutoverReport> {
    let resolved = resolve(options)?;
    plan_resolved(&resolved).await
}

async fn plan_resolved(resolved: &ResolvedMemoryCutover) -> Result<MemoryCutoverReport> {
    let scratch = resolved.data_root.join("scratch").join("memory-cutover");
    storage::PrivateStoreIo::create_dir_all(&scratch)?;
    let mut sources = Vec::new();
    for path in branch_database_paths(&resolved.data_root)? {
        let snapshot = tracedecay_runtime_core::sqlite_read_snapshot::open_in(&path, &scratch)
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
        let memory_v2_fact_count = table_count(snapshot.connection(), "memory_v2_facts").await?;
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
            memory_v2_fact_count,
        });
    }
    let confirmation_token =
        confirmation_token(&resolved.project_id, &resolved.graph_db_path, &sources);
    Ok(MemoryCutoverReport {
        project_id: resolved.project_id.clone(),
        project_graph: resolved.graph_db_path.clone(),
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
    let resolved = resolve(options)?;
    let planned = plan_resolved(&resolved).await?;
    if planned.confirmation_token != expected_confirmation_token {
        return Err(migration_error(
            "memory cutover confirmation token does not match the current branch-store generation",
        ));
    }
    let lifecycle = tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_for_profile(
        &resolved.profile_root,
        "project-wide branch memory cutover",
    )?;
    let _database_scope = tracedecay_runtime_core::db::enter_maintenance_database_scope(
        &lifecycle,
        &resolved.profile_root,
        "project-wide branch memory cutover",
    )?;
    let identity = crate::profile_identity::load_or_create(&resolved.profile_root)?;
    let runtime = crate::session_runtime::DaemonSessionRuntimeRegistryV1::open(identity).await?;
    let project_id = ProjectId::new(resolved.project_id.clone())
        .map_err(|error| migration_error(error.to_string()))?;
    let target = runtime
        .project_memory(project_id, [resolved.project_root.clone()])
        .await?;
    apply_planned(&resolved, &target, planned).await
}

/// Runs the same generation-bound cutover as the offline migration command
/// against a daemon-retained project graph. The caller must exclude branch and
/// project-store writers for the duration; source generations are still
/// verified before the receipt is published, so external or ambiguous changes
/// fail closed.
pub async fn apply_for_retained_project(
    project_root: &Path,
    profile_root: &Path,
    store_layout: &storage::StoreLayout,
    target: &tracedecay_runtime_core::db::Database,
) -> Result<MemoryCutoverReport> {
    let options = MemoryCutoverOptions {
        project_root: project_root.to_path_buf(),
        profile_root: profile_root.to_path_buf(),
    };
    let resolved = resolve(&options)?;
    if store_layout.data_root != resolved.data_root
        || store_layout.graph_db_path != resolved.graph_db_path
    {
        return Err(migration_error(
            "retained project graph does not match the resolved project-memory cutover store",
        ));
    }
    let planned = plan_resolved(&resolved).await?;
    apply_planned(&resolved, target, planned).await
}

async fn apply_planned(
    resolved: &ResolvedMemoryCutover,
    target: &tracedecay_runtime_core::db::Database,
    planned: MemoryCutoverReport,
) -> Result<MemoryCutoverReport> {
    let scratch = resolved.data_root.join("scratch").join("memory-cutover");
    storage::PrivateStoreIo::create_dir_all(&scratch)?;
    let mut archive_proofs = Vec::with_capacity(planned.sources.len());
    for source in &planned.sources {
        if source_generation(&source.path)? != source.generation {
            return Err(migration_error(format!(
                "branch memory source '{}' changed after planning",
                source.path.display()
            )));
        }
        let snapshot =
            tracedecay_runtime_core::sqlite_read_snapshot::open_in(&source.path, &scratch)
                .await
                .map_err(|error| {
                    migration_error(format!("snapshot '{}': {error}", source.path.display()))
                })?;
        let proofs =
            crate::consolidate::sqlite::merge_branch_legacy_memory_snapshot(target, &snapshot)
                .await?;
        if source.memory_v2_fact_count > 0 && proofs.is_empty() {
            return Err(migration_error(format!(
                "branch memory source '{}' has V2 authority without an archive inclusion proof",
                source.path.display()
            )));
        }
        archive_proofs.push((source.path.clone(), proofs));
    }
    crate::consolidate::sqlite::rebuild_branch_cutover_memory_banks(target).await?;

    let owner = FactOwnerV1::Project {
        project_id: ProjectId::new(resolved.project_id.clone())
            .map_err(|error| migration_error(error.to_string()))?,
    };
    let source_store_id = SourceStoreId::new(LEGACY_SOURCE_STORE.to_owned())
        .map_err(|error| migration_error(error.to_string()))?;
    target
        .reopen_memory_v2_cutover_for_legacy_union(&owner, &source_store_id)
        .await?;

    let cutover = CompatibilityLegacyMemoryCutoverCommandV1::new(
        owner,
        ProvenanceId::new("v1-cutover".to_owned())
            .map_err(|error| migration_error(error.to_string()))?,
    )
    .map_err(|error| migration_error(error.to_string()))?;
    let store = DatabaseFactStore::new(target);
    let mut cutover_passes = 0;
    loop {
        cutover_passes += 1;
        if cutover_passes > MAX_CUTOVER_PASSES {
            return Err(migration_error(
                "memory cutover exceeded its bounded pass limit",
            ));
        }
        if store
            .advance_compatibility_legacy_memory_cutover(cutover.clone())
            .await
            .map_err(|error| migration_error(error.to_string()))?
            == CompatibilityLegacyMemoryCutoverProgressV1::Complete
        {
            break;
        }
    }
    verify_source_generations(&planned.sources)?;
    target.checkpoint().await?;
    inject_cutover_fault(CutoverFaultPhase::TargetDurabilityBarrier)?;
    storage::PrivateStoreIo::sync_sqlite_family(&resolved.graph_db_path)
        .map_err(|error| migration_error(format!("synchronize project memory target: {error}")))?;
    write_cutover_receipt(resolved, &planned.sources, &archive_proofs)?;

    Ok(MemoryCutoverReport {
        applied: true,
        cutover_passes,
        ..planned
    })
}

#[derive(Clone, Copy)]
enum CutoverFaultPhase {
    TargetDurabilityBarrier = 1,
}

fn inject_cutover_fault(_phase: CutoverFaultPhase) -> Result<()> {
    #[cfg(any(test, feature = "test-transport"))]
    if CUTOVER_FAULT
        .compare_exchange(_phase as u8, 0, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        return Err(migration_error(match _phase {
            CutoverFaultPhase::TargetDurabilityBarrier => {
                "injected target durability barrier failure"
            }
        }));
    }
    Ok(())
}

/// Fails closed unless every selected branch family exactly matches a source
/// generation covered by a completed project-wide memory cutover.
pub fn verify_branch_removal_receipts(
    data_root: &Path,
    original_paths: &[PathBuf],
    validation_paths: &[PathBuf],
) -> Result<()> {
    let receipt = read_cutover_receipt(data_root)?;
    // Callers select branch families from unordered sets. Verification refuses
    // on the first uncovered family, so a stable order is what makes the
    // refusal itself deterministic; sorting here keeps that property at the
    // chokepoint instead of at every call site.
    let mut original_paths = original_paths.to_vec();
    original_paths.sort();
    for original in &original_paths {
        let relative = original.strip_prefix(data_root).map_err(|_| {
            migration_error(format!(
                "branch database '{}' escapes its project store",
                original.display()
            ))
        })?;
        let candidate = branch_validation_candidate(original, validation_paths)?;
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
        let v2_authority_tables = branch_v2_authority_tables(&candidate);
        let has_v2_authority = !v2_authority_tables.is_empty();
        if has_v2_authority && (expected.legacy_only || expected.archives.is_empty()) {
            return Err(migration_error(format!(
                "branch database '{}' has V2 authority in {:?} without a digest-bound archive receipt",
                original.display(),
                v2_authority_tables,
            )));
        }
        for proof in &expected.archives {
            let owner_matches = matches!(
                &proof.owner,
                FactOwnerV1::Project { project_id }
                    if project_id.as_str() == receipt
                        .as_ref()
                        .map(|receipt| receipt.project_id.as_str())
                        .unwrap_or_default()
            );
            if proof.schema != MEMORY_V2_OWNER_ARCHIVE_SCHEMA_V1
                || !owner_matches
                || !is_sha256_digest(&proof.source_digest)
                || !is_sha256_digest(&proof.target_digest)
            {
                return Err(migration_error(format!(
                    "branch database '{}' has an incomplete archive inclusion proof",
                    original.display()
                )));
            }
        }
    }
    verify_branch_removal_archive_closure_blocking(data_root, &original_paths, validation_paths)
}

fn verify_branch_removal_archive_closure_blocking(
    data_root: &Path,
    original_paths: &[PathBuf],
    validation_paths: &[PathBuf],
) -> Result<()> {
    let data_root = data_root.to_path_buf();
    let original_paths = original_paths.to_vec();
    let validation_paths = validation_paths.to_vec();
    std::thread::Builder::new()
        .name("memory-cutover-removal-audit".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    migration_error(format!(
                        "create branch-removal archive verification runtime: {error}"
                    ))
                })?;
            runtime.block_on(verify_branch_removal_archive_closure(
                &data_root,
                &original_paths,
                &validation_paths,
            ))
        })
        .map_err(|error| {
            migration_error(format!(
                "start branch-removal archive verification: {error}"
            ))
        })?
        .join()
        .map_err(|_| migration_error("branch-removal archive verification panicked"))?
}

async fn verify_branch_removal_archive_closure(
    data_root: &Path,
    original_paths: &[PathBuf],
    validation_paths: &[PathBuf],
) -> Result<()> {
    let Some(receipt) = read_cutover_receipt(data_root)? else {
        return Ok(());
    };
    if receipt
        .sources
        .iter()
        .all(|source| source.archives.is_empty())
    {
        return Ok(());
    }

    let scratch = data_root.join("scratch").join("memory-cutover-removal");
    storage::PrivateStoreIo::create_dir_all(&scratch)?;
    let target_path = data_root.join(tracedecay_runtime_core::config::db_filename(data_root));
    let target = tracedecay_runtime_core::sqlite_read_snapshot::open_in(&target_path, &scratch)
        .await
        .map_err(|error| {
            migration_error(format!(
                "snapshot current project memory target '{}': {error}",
                target_path.display()
            ))
        })?;

    let mut original_paths = original_paths.to_vec();
    original_paths.sort();
    for original in &original_paths {
        let relative = original.strip_prefix(data_root).map_err(|_| {
            migration_error(format!(
                "branch database '{}' escapes its project store",
                original.display()
            ))
        })?;
        let Some(expected) = receipt
            .sources
            .iter()
            .find(|source| source.relative_path == relative)
        else {
            continue;
        };
        if expected.archives.is_empty() {
            continue;
        }

        let candidate = branch_validation_candidate(original, validation_paths)?;
        if source_generation(&candidate)? != expected.generation {
            return Err(migration_error(format!(
                "branch database '{}' changed after project-memory cutover; deletion refused",
                original.display()
            )));
        }
        let source = tracedecay_runtime_core::sqlite_read_snapshot::open_in(&candidate, &scratch)
            .await
            .map_err(|error| {
                migration_error(format!(
                    "snapshot branch memory source '{}': {error}",
                    candidate.display()
                ))
            })?;
        let source_owners =
            list_memory_v2_archive_owners(source.connection(), MemoryV2ArchiveDatabase::Main)
                .await?;
        let source_owner_keys = source_owners
            .iter()
            .map(owner_key)
            .collect::<Result<BTreeSet<_>>>()?;
        let proof_owner_keys = expected
            .archives
            .iter()
            .map(|proof| owner_key(&proof.owner))
            .collect::<Result<BTreeSet<_>>>()?;
        if source_owner_keys != proof_owner_keys
            || proof_owner_keys.len() != expected.archives.len()
        {
            return Err(migration_error(format!(
                "branch database '{}' archive receipt does not cover every current owner",
                original.display()
            )));
        }

        for proof in &expected.archives {
            validate_archive_proof(&receipt, original, proof)?;
            let source_archive = export_memory_v2_owner_archive(
                source.connection(),
                MemoryV2ArchiveDatabase::Main,
                &proof.owner,
            )
            .await?;
            let source_digest = source_archive
                .digest()
                .map_err(|error| migration_error(error.to_string()))?;
            if source_digest.as_str() != proof.source_digest.as_str() {
                return Err(migration_error(format!(
                    "branch database '{}' current archive digest does not match its cutover receipt",
                    original.display()
                )));
            }
            let target_archive = export_memory_v2_owner_archive(
                target.connection(),
                MemoryV2ArchiveDatabase::Main,
                &proof.owner,
            )
            .await?;
            let plan = plan_memory_v2_owner_merge(&source_archive, &target_archive)
                .map_err(|error| migration_error(error.to_string()))?;
            if !plan.inserts().is_empty()
                || !plan.updates().is_empty()
                || !plan.conflicts().is_empty()
            {
                return Err(migration_error(format!(
                    "current project memory target does not contain the complete archive from branch database '{}': inserts={}, updates={}, conflicts={}",
                    original.display(),
                    plan.inserts().len(),
                    plan.updates().len(),
                    plan.conflicts().len()
                )));
            }
        }
        source
            .validate_source()
            .map_err(|error| migration_error(error.to_string()))?;
    }
    target
        .validate_source()
        .map_err(|error| migration_error(error.to_string()))
}

fn read_cutover_receipt(data_root: &Path) -> Result<Option<BranchMemoryCutoverReceipt>> {
    let receipt_path = data_root.join(RECEIPT_FILENAME);
    let receipt: BranchMemoryCutoverReceipt = match fs::read(&receipt_path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
            migration_error(format!(
                "project-memory cutover receipt '{}' is invalid: {error}",
                receipt_path.display()
            ))
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(migration_error(format!(
                "cannot read project-memory cutover receipt '{}': {error}",
                receipt_path.display()
            )));
        }
    };
    if receipt.version != 2 || receipt.archive_schema != MEMORY_V2_OWNER_ARCHIVE_SCHEMA_V1 {
        return Err(migration_error(
            "unsupported project-memory cutover receipt",
        ));
    }
    let manifest_path = data_root.join(storage::STORE_MANIFEST_FILENAME);
    if manifest_path.is_file() {
        let manifest = storage::read_store_manifest(&manifest_path)?;
        if manifest.project_id.as_deref() != Some(receipt.project_id.as_str()) {
            return Err(migration_error(
                "project-memory cutover receipt belongs to a different project",
            ));
        }
    }
    Ok(Some(receipt))
}

fn branch_validation_candidate(original: &Path, validation_paths: &[PathBuf]) -> Result<PathBuf> {
    if original.exists() {
        return Ok(original.to_path_buf());
    }
    let original_name = original
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| migration_error("branch database filename is not UTF-8"))?;
    let matches_quarantine = |path: &Path| {
        path.exists()
            && path.parent() == original.parent()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with(&format!(".{original_name}.branch-delete-"))
                        && name.ends_with(".quarantine")
                })
    };
    let mut candidates = validation_paths
        .iter()
        .filter(|path| matches_quarantine(path))
        .cloned()
        .collect::<BTreeSet<_>>();
    if let Some(parent) = original.parent() {
        for entry in fs::read_dir(parent).map_err(|error| {
            migration_error(format!(
                "inspect quarantined branch families in '{}': {error}",
                parent.display()
            ))
        })? {
            let path = entry
                .map_err(|error| migration_error(error.to_string()))?
                .path();
            if matches_quarantine(&path) {
                candidates.insert(path);
            }
        }
    }
    if candidates.len() != 1 {
        return Err(migration_error(format!(
            "cannot locate exactly one quarantined family for '{}'",
            original.display()
        )));
    }
    candidates
        .pop_first()
        .ok_or_else(|| migration_error("quarantine candidate disappeared"))
}

fn validate_archive_proof(
    receipt: &BranchMemoryCutoverReceipt,
    original: &Path,
    proof: &crate::consolidate::sqlite::MemoryV2ArchiveMergeProof,
) -> Result<()> {
    let owner_matches = matches!(
        &proof.owner,
        FactOwnerV1::Project { project_id } if project_id.as_str() == receipt.project_id
    );
    if proof.schema != MEMORY_V2_OWNER_ARCHIVE_SCHEMA_V1
        || !owner_matches
        || !is_sha256_digest(&proof.source_digest)
        || !is_sha256_digest(&proof.target_digest)
    {
        return Err(migration_error(format!(
            "branch database '{}' has an incomplete archive inclusion proof",
            original.display()
        )));
    }
    Ok(())
}

fn owner_key(owner: &FactOwnerV1) -> Result<String> {
    serde_json::to_string(owner).map_err(|error| migration_error(error.to_string()))
}

fn branch_v2_authority_tables(path: &Path) -> Vec<&'static str> {
    [
        "memory_v2_facts",
        "memory_v2_assertions",
        "memory_v2_lineage_events",
        "memory_v2_evidence",
        "memory_v2_feedback_history",
        "memory_v2_fact_relations",
        "memory_v2_proposals",
        "memory_v2_proposal_transitions",
        "memory_v2_legacy_map",
        "memory_v2_legacy_quarantine",
        "memory_v2_compatibility_operation_receipts",
    ]
    .into_iter()
    .filter(|table| {
        tracedecay_runtime_core::sqlite_read_snapshot::checkpointed_database_has_any_rows(
            path,
            &[*table],
        )
        .is_ok_and(|has_rows| has_rows)
    })
    .collect()
}

fn is_sha256_digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn branch_has_no_durable_memory(path: &Path) -> bool {
    tracedecay_runtime_core::sqlite_read_snapshot::checkpointed_database_has_any_rows(
        path,
        &[
            "memory_facts",
            "memory_feedback_events",
            "memory_oplog",
            "memory_v2_facts",
            "memory_v2_assertions",
            "memory_v2_lineage_events",
            "memory_v2_evidence",
            "memory_v2_feedback_history",
            "memory_v2_fact_relations",
            "memory_v2_proposals",
            "memory_v2_proposal_transitions",
            "memory_v2_legacy_quarantine",
            "memory_v2_compatibility_operation_receipts",
        ],
    )
    .is_ok_and(|has_rows| !has_rows)
}

struct ResolvedMemoryCutover {
    project_root: PathBuf,
    profile_root: PathBuf,
    data_root: PathBuf,
    graph_db_path: PathBuf,
    project_id: String,
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
    branch_meta::load_branch_meta(&layout.data_root)
        .ok_or_else(|| migration_error("branch metadata is required for memory cutover"))?;
    Ok(ResolvedMemoryCutover {
        project_root,
        profile_root,
        data_root: layout.data_root,
        graph_db_path: layout.graph_db_path,
        project_id: marker.project_id,
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
    format!("confirm-memory-cutover-{}", hex::encode(hasher.finalize()))
}

fn source_generation(path: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.sqlite-family-generation.v1\0");
    // The database and WAL are durable state. SHM is a rebuildable coordination
    // file whose bytes can change during a read-only snapshot; binding it would
    // invalidate a confirmation token without any durable memory change.
    for suffix in ["", "-wal"] {
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
                    hasher.update(metadata.dev().to_le_bytes());
                    hasher.update(metadata.ino().to_le_bytes());
                }
                let mut file =
                    fs::File::open(&member).map_err(|error| migration_error(error.to_string()))?;
                let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
                loop {
                    let read = file
                        .read(&mut buffer)
                        .map_err(|error| migration_error(error.to_string()))?;
                    if read == 0 {
                        break;
                    }
                    hasher.update(&buffer[..read]);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => hasher.update([0]),
            Err(error) => return Err(migration_error(error.to_string())),
        }
    }
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

fn verify_source_generations(sources: &[MemoryCutoverSource]) -> Result<()> {
    for source in sources {
        if source_generation(&source.path)? != source.generation {
            return Err(migration_error(format!(
                "branch memory source '{}' changed during cutover",
                source.path.display()
            )));
        }
    }
    Ok(())
}

fn write_cutover_receipt(
    resolved: &ResolvedMemoryCutover,
    sources: &[MemoryCutoverSource],
    archive_proofs: &[(
        PathBuf,
        Vec<crate::consolidate::sqlite::MemoryV2ArchiveMergeProof>,
    )],
) -> Result<()> {
    let receipt = BranchMemoryCutoverReceipt {
        version: 2,
        project_id: resolved.project_id.clone(),
        archive_schema: MEMORY_V2_OWNER_ARCHIVE_SCHEMA_V1.to_owned(),
        completed_at: tracedecay_runtime_core::tracedecay::current_timestamp(),
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
                let proofs = archive_proofs
                    .iter()
                    .find(|(path, _)| path == &source.path)
                    .map(|(_, proofs)| proofs.clone())
                    .ok_or_else(|| {
                        migration_error(format!(
                            "branch source '{}' has no archive proof result",
                            source.path.display()
                        ))
                    })?;
                for proof in &proofs {
                    if proof.schema != MEMORY_V2_OWNER_ARCHIVE_SCHEMA_V1
                        || !matches!(
                            &proof.owner,
                            FactOwnerV1::Project { project_id }
                                if project_id.as_str() == resolved.project_id
                        )
                    {
                        return Err(migration_error(
                            "Memory V2 archive proof has an incompatible schema or project owner",
                        ));
                    }
                }
                Ok(BranchMemoryCutoverReceiptSource {
                    relative_path,
                    generation: source.generation.clone(),
                    legacy_only: proofs.is_empty(),
                    archives: proofs,
                })
            })
            .collect::<Result<Vec<_>>>()?,
    };
    let path = resolved.data_root.join(RECEIPT_FILENAME);
    let temp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    let bytes =
        serde_json::to_vec_pretty(&receipt).map_err(|error| migration_error(error.to_string()))?;
    storage::PrivateStoreIo::write_file_atomically_durable(&path, &temp, &bytes)
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
            tracedecay_runtime_core::db::engine::params![table],
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tracedecay_runtime_core::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};

    /// A project store holding one tracked branch whose `SQLite` family carries a
    /// durable fact that exists nowhere else.
    struct BranchStoreFixture {
        _temp: tempfile::TempDir,
        data_root: PathBuf,
    }

    impl BranchStoreFixture {
        fn new(branches: &[&str]) -> Self {
            let fixture = Self::empty(branches);
            for branch in branches {
                fixture.seed_branch_only_fact(branch);
            }
            fixture
        }

        fn empty(branches: &[&str]) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let data_root = temp.path().join("store");
            fs::create_dir_all(data_root.join("branches")).unwrap();
            let mut meta = branch_meta::BranchMeta::new("main");
            for branch in branches {
                meta.add_branch(branch, &format!("branches/{branch}.db"), "main");
            }
            branch_meta::save_branch_meta(&data_root, &meta).unwrap();

            Self {
                _temp: temp,
                data_root,
            }
        }

        fn database_path(&self, branch: &str) -> PathBuf {
            self.data_root.join(format!("branches/{branch}.db"))
        }

        /// Writes a fact that lives only in this branch store, mirroring the two
        /// damaged branch stores whose facts existed nowhere else.
        fn seed_branch_only_fact(&self, branch: &str) {
            let connection = rusqlite::Connection::open(self.database_path(branch)).unwrap();
            connection
                .execute_batch(&format!(
                    "CREATE TABLE memory_facts(
                         fact_id INTEGER PRIMARY KEY,
                         content TEXT NOT NULL
                     );
                     INSERT INTO memory_facts(fact_id, content)
                     VALUES(1, 'branch-exclusive durable fact for {branch}');"
                ))
                .unwrap();
        }

        fn branch_only_fact_count(&self, branch: &str) -> i64 {
            let connection = rusqlite::Connection::open_with_flags(
                self.database_path(branch),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            connection
                .query_row("SELECT COUNT(*) FROM memory_facts", [], |row| row.get(0))
                .unwrap()
        }

        fn write_receipt(&self, sources: Vec<BranchMemoryCutoverReceiptSource>) {
            let receipt = BranchMemoryCutoverReceipt {
                version: 2,
                project_id: "fixture-project".to_owned(),
                archive_schema: MEMORY_V2_OWNER_ARCHIVE_SCHEMA_V1.to_owned(),
                completed_at: 1,
                sources,
            };
            fs::write(
                self.data_root.join(RECEIPT_FILENAME),
                serde_json::to_vec_pretty(&receipt).unwrap(),
            )
            .unwrap();
        }

        fn covering_source(&self, branch: &str) -> BranchMemoryCutoverReceiptSource {
            BranchMemoryCutoverReceiptSource {
                relative_path: PathBuf::from(format!("branches/{branch}.db")),
                generation: source_generation(&self.database_path(branch)).unwrap(),
                legacy_only: true,
                archives: Vec::new(),
            }
        }

        fn verify_branch_removal(&self, branch: &str) -> Result<()> {
            let database_path = self.database_path(branch);
            verify_branch_removal_receipts(
                &self.data_root,
                std::slice::from_ref(&database_path),
                std::slice::from_ref(&database_path),
            )
        }
    }

    async fn initialize_test_database(path: &Path) -> Database {
        // The kernel initialises the profile sidecar shard through a
        // fail-closed port whose real installer lives in `tracedecay-global-db`.
        // Idempotent — the port keeps the first registration.
        tracedecay_global_db::register_test_schema_installer();
        let authority =
            DatabaseAuthority::acquire_test(path, "memory cutover removal test").unwrap();
        Database::publish_test_runtime(path, &authority, TestDatabaseRuntimeMode::Initialize)
            .await
            .unwrap()
            .0
    }

    #[test]
    fn branch_removal_without_receipt_refuses_and_preserves_branch_only_fact() {
        let fixture = BranchStoreFixture::new(&["feature"]);

        let error = fixture
            .verify_branch_removal("feature")
            .expect_err("removal must refuse without a covering cutover receipt");

        assert!(
            error
                .to_string()
                .contains("no completed project-memory cutover receipt"),
            "{error}"
        );
        assert!(fixture.database_path("feature").exists());
        assert_eq!(fixture.branch_only_fact_count("feature"), 1);
        assert!(
            branch_meta::load_branch_meta(&fixture.data_root)
                .unwrap()
                .is_tracked("feature")
        );
    }

    #[test]
    fn branch_removal_with_generation_bound_receipt_validates() {
        let fixture = BranchStoreFixture::new(&["feature"]);
        fixture.write_receipt(vec![fixture.covering_source("feature")]);

        fixture
            .verify_branch_removal("feature")
            .expect("a generation-bound receipt must authorize removal");
    }

    #[test]
    fn branch_removal_refuses_a_receipt_bound_to_a_stale_generation() {
        let fixture = BranchStoreFixture::new(&["feature"]);
        fixture.write_receipt(vec![BranchMemoryCutoverReceiptSource {
            relative_path: PathBuf::from("branches/feature.db"),
            generation: "sha256:0000000000000000000000000000000000000000000000000000000000000000"
                .to_owned(),
            legacy_only: true,
            archives: Vec::new(),
        }]);

        let error = fixture
            .verify_branch_removal("feature")
            .expect_err("a receipt from a different generation must not authorize removal");

        assert!(
            error
                .to_string()
                .contains("changed after project-memory cutover"),
            "{error}"
        );
        assert_eq!(fixture.branch_only_fact_count("feature"), 1);
    }

    #[tokio::test]
    async fn branch_removal_refuses_when_archived_target_closure_is_missing() {
        let fixture = BranchStoreFixture::empty(&["feature"]);
        let branch_path = fixture.database_path("feature");
        let target_path = fixture
            .data_root
            .join(tracedecay_runtime_core::config::DB_FILENAME);
        let target = initialize_test_database(&target_path).await;
        let source = initialize_test_database(&branch_path).await;
        let owner = FactOwnerV1::Project {
            project_id: ProjectId::new("fixture-project").unwrap(),
        };
        let source_write = source
            .begin_write_transaction("seed branch-only V2 archive")
            .await
            .unwrap();
        source_write
            .execute(
                "INSERT INTO memory_v2_facts(
                    fact_id, owner_kind, project_id, owner_json, identity_json, created_at
                 ) VALUES (?1, 'project', ?2, ?3, ?4, 1)",
                tracedecay_runtime_core::db::engine::params![
                    "fact.branch-only",
                    "fixture-project",
                    serde_json::to_string(&owner).unwrap(),
                    "{\"source\":\"branch-only\"}"
                ],
            )
            .await
            .unwrap();
        source_write.commit().await.unwrap();
        source.checkpoint().await.unwrap();
        source.close();

        let scratch = fixture.data_root.join("scratch").join("test-cutover");
        storage::PrivateStoreIo::create_dir_all(&scratch).unwrap();
        let snapshot =
            tracedecay_runtime_core::sqlite_read_snapshot::open_in(&branch_path, &scratch)
                .await
                .unwrap();
        let proofs =
            crate::consolidate::sqlite::merge_branch_legacy_memory_snapshot(&target, &snapshot)
                .await
                .unwrap();
        snapshot.validate_source().unwrap();
        drop(snapshot);
        target.checkpoint().await.unwrap();
        fixture.write_receipt(vec![BranchMemoryCutoverReceiptSource {
            relative_path: PathBuf::from("branches/feature.db"),
            generation: source_generation(&branch_path).unwrap(),
            legacy_only: false,
            archives: proofs,
        }]);

        target.checkpoint().await.unwrap();
        target.close();
        let target_connection = rusqlite::Connection::open(&target_path).unwrap();
        target_connection
            .execute_batch(
                "DROP TRIGGER memory_v2_facts_no_delete;
                 DELETE FROM memory_v2_facts WHERE fact_id = 'fact.branch-only';",
            )
            .unwrap();
        drop(target_connection);

        let error = fixture
            .verify_branch_removal("feature")
            .expect_err("a receipt cannot replace current target-closure proof");

        assert!(
            error
                .to_string()
                .contains("current project memory target does not contain"),
            "{error}"
        );
        assert!(branch_path.exists());
    }

    #[test]
    fn receipt_verification_is_deterministic_regardless_of_path_order() {
        let fixture = BranchStoreFixture::new(&["alpha", "zeta"]);
        // `alpha` sorts first and fails on a stale generation; `zeta` fails on a
        // missing receipt. Whichever path is inspected first decides the
        // reported refusal, so an unordered caller would report either one.
        fixture.write_receipt(vec![BranchMemoryCutoverReceiptSource {
            relative_path: PathBuf::from("branches/alpha.db"),
            generation: "sha256:0000000000000000000000000000000000000000000000000000000000000000"
                .to_owned(),
            legacy_only: true,
            archives: Vec::new(),
        }]);
        let alpha = fixture.database_path("alpha");
        let zeta = fixture.database_path("zeta");

        let forward = verify_branch_removal_receipts(
            &fixture.data_root,
            &[alpha.clone(), zeta.clone()],
            &[alpha.clone(), zeta.clone()],
        )
        .expect_err("uncovered branch families must refuse removal");
        let reversed = verify_branch_removal_receipts(
            &fixture.data_root,
            &[zeta.clone(), alpha.clone()],
            &[zeta, alpha],
        )
        .expect_err("uncovered branch families must refuse removal");

        assert_eq!(forward.to_string(), reversed.to_string());
        assert!(
            forward
                .to_string()
                .contains("changed after project-memory cutover"),
            "{forward}"
        );
    }
}
