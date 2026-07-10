//! Explicit, offline consolidation of two profile shards for one repository.
//!
//! The runtime deliberately refuses to guess when both shards contain data.
//! This workflow builds a third deterministic shard, preserving both inputs
//! and cutting the repository marker over only after the new shard and global
//! registry have verified successfully.

mod evidence;
mod files;
mod finalize;
mod preflight;
mod prepare;
mod sqlite;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use evidence::{GraphStoreEvidence, InputReadEvidence, capture_input_evidence};
#[cfg(test)]
use files::sqlite_sidecar;
use files::{
    copy_file_atomic, copy_sqlite_family_exact, copy_tree_exact, excluded_source_artifact,
    file_digest, is_reference_artifact, is_runtime_lock, is_sqlite_database, is_sqlite_sidecar,
    relative_file_map, tree_stats,
};
use finalize::{cut_over_markers, register_destination, verify_destination};
use preflight::{acquire_store_locks, ensure_profile_offline, preflight_disk_space};
use prepare::prepare_destination;

use crate::branch_meta::{self, BranchMeta};
use crate::errors::{Result, TraceDecayError};
use crate::global_db::{GlobalDb, GraphScopeUpsert, StoreArtifactUpsert, StoreInstanceUpsert};
use crate::storage::{
    self, EnrollmentMarker, PrivateStoreIo, StorageMode, StoreKind, StoreLayout, StoreManifest,
};

const LEDGER_SCHEMA_VERSION: u32 = 1;
const BACKUP_DIR: &str = "migration-backups";
const LEDGER_DIR: &str = "migration-inventory";
const PRESERVED_DIR: &str = "consolidation-preserved";
const INPUT_DIR: &str = ".consolidation-input";

#[derive(Debug, Clone)]
pub struct ConsolidationOptions {
    pub project_root: PathBuf,
    pub profile_root: PathBuf,
    pub source_project_id: String,
    pub target_project_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConsolidationState {
    Planned,
    BackupsReady,
    DestinationReady,
    DatabasesMerged,
    ArtifactsMerged,
    Registered,
    Applied,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreInventory {
    pub project_id: String,
    pub data_root: PathBuf,
    pub graph_databases: usize,
    pub facts: u64,
    pub feedback_events: u64,
    pub sessions: u64,
    pub messages: u64,
    pub lcm_raw_messages: u64,
    pub branches: usize,
    pub artifact_files: usize,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollisionSummary {
    pub fact_content_overlaps: u64,
    pub session_overlaps: u64,
    pub message_overlaps: u64,
    pub lcm_message_overlaps: u64,
    pub artifact_path_overlaps: usize,
    pub differing_artifact_paths: Vec<PathBuf>,
    pub semantics: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidationReport {
    pub migration_id: String,
    pub state: ConsolidationState,
    pub project_root: PathBuf,
    pub git_common_dir: PathBuf,
    pub source: StoreInventory,
    pub target: StoreInventory,
    pub destination_project_id: String,
    pub destination_data_root: PathBuf,
    pub backup_root: PathBuf,
    pub ledger_path: PathBuf,
    pub confirmation_token: String,
    pub collisions: CollisionSummary,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConsolidationLedger {
    schema_version: u32,
    migration_id: String,
    confirmation_token: String,
    input_fingerprint: String,
    source_project_id: String,
    target_project_id: String,
    destination_project_id: String,
    project_root: PathBuf,
    git_common_dir: PathBuf,
    state: ConsolidationState,
    graph_offsets: Vec<sqlite::GraphMergeOffsets>,
    session_offsets: Option<sqlite::SessionMergeOffsets>,
    preserved_collisions: Vec<PathBuf>,
}

struct ResolvedPlan {
    report: ConsolidationReport,
    input_fingerprint: String,
    source_layout: StoreLayout,
    target_layout: StoreLayout,
    source_meta: BranchMeta,
    target_meta: BranchMeta,
    scratch_root: MigrationScratchRoot,
    evidence: Arc<InputReadEvidence>,
}

static NEXT_MIGRATION_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct MigrationScratchRoot {
    path: PathBuf,
}

impl MigrationScratchRoot {
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for MigrationScratchRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub async fn plan(options: &ConsolidationOptions) -> Result<ConsolidationReport> {
    ensure_profile_offline(options)?;
    Ok(resolve_plan(options).await?.report)
}

pub async fn apply(
    options: &ConsolidationOptions,
    confirmation_token: &str,
) -> Result<ConsolidationReport> {
    apply_with_stop(options, confirmation_token, None).await
}

async fn apply_with_stop(
    options: &ConsolidationOptions,
    confirmation_token: &str,
    stop_after: Option<ConsolidationState>,
) -> Result<ConsolidationReport> {
    apply_with_faults(options, confirmation_token, stop_after, None).await
}

#[cfg(test)]
async fn apply_with_prepare_stop(
    options: &ConsolidationOptions,
    confirmation_token: &str,
    prepare_stop: prepare::PrepareStop,
) -> Result<ConsolidationReport> {
    apply_with_faults(options, confirmation_token, None, Some(prepare_stop)).await
}

async fn apply_with_faults(
    options: &ConsolidationOptions,
    confirmation_token: &str,
    stop_after: Option<ConsolidationState>,
    prepare_stop: Option<prepare::PrepareStop>,
) -> Result<ConsolidationReport> {
    ensure_profile_offline(options)?;
    let _lifecycle = crate::lifecycle_lease::acquire_exclusive_for_profile(
        &options.profile_root,
        "profile shard consolidation",
    )?;
    let resolved = resolve_plan_allowing_applied(options).await?;
    if resolved.report.confirmation_token != confirmation_token {
        return Err(config_error(format!(
            "confirmation token mismatch; rerun the dry-run and pass --confirm-token {}",
            resolved.report.confirmation_token
        )));
    }
    preflight_disk_space(&resolved)?;
    let _store_locks = acquire_store_locks(&resolved.source_layout, &resolved.target_layout)?;
    let guarded_paths = input_database_paths(&resolved)?;
    let source_graphs = graph_db_paths(&resolved.source_layout, &resolved.source_meta)?;
    let target_graphs = graph_db_paths(&resolved.target_layout, &resolved.target_meta)?;
    let session_paths = vec![
        resolved.source_layout.sessions_db_path.clone(),
        resolved.target_layout.sessions_db_path.clone(),
    ];
    resolved
        .evidence
        .validate(&source_graphs, &target_graphs, &session_paths)?;
    let _database_guards = sqlite::acquire_offline_guards(&guarded_paths).await?;
    // Advisory store locks do not cover old or direct MCP writers. Recompute
    // under SQLite write reservations so the token and backups describe the
    // exact frozen inputs used below.
    let locked = resolve_plan_inner(options, true, Some(Arc::clone(&resolved.evidence))).await?;
    if locked.report.confirmation_token != confirmation_token {
        return Err(config_error(format!(
            "input stores changed after the dry-run; rerun it and pass --confirm-token {}",
            locked.report.confirmation_token
        )));
    }
    let resolved = locked;
    preflight_disk_space(&resolved)?;
    let ledger_path = resolved.report.ledger_path.clone();
    let mut ledger = load_or_create_ledger(&resolved, &ledger_path)?;
    validate_ledger(&ledger, &resolved)?;
    if ledger.state == ConsolidationState::Applied {
        let mut report = resolved.report;
        report.state = ConsolidationState::Applied;
        report.dry_run = false;
        return Ok(report);
    }

    if ledger.state == ConsolidationState::Planned {
        backup_store(&resolved.source_layout, &resolved.report.backup_root)?;
        backup_store(&resolved.target_layout, &resolved.report.backup_root)?;
        ledger.state = ConsolidationState::BackupsReady;
        save_ledger(&ledger_path, &ledger)?;
        maybe_stop(&ledger.state, stop_after.as_ref())?;
    }

    if ledger.state == ConsolidationState::BackupsReady {
        if prepare_stop.is_some() {
            prepare::prepare_destination_with_stop(&resolved, prepare_stop)?;
        } else {
            prepare_destination(&resolved)?;
        }
        ledger.state = ConsolidationState::DestinationReady;
        save_ledger(&ledger_path, &ledger)?;
        maybe_stop(&ledger.state, stop_after.as_ref())?;
    }

    if ledger.state == ConsolidationState::DestinationReady {
        merge_databases(&resolved, &mut ledger).await?;
        ledger.state = ConsolidationState::DatabasesMerged;
        save_ledger(&ledger_path, &ledger)?;
        maybe_stop(&ledger.state, stop_after.as_ref())?;
    }

    if ledger.state == ConsolidationState::DatabasesMerged {
        merge_non_database_artifacts(&resolved, &mut ledger)?;
        write_destination_manifest(&resolved)?;
        let session_offsets = ledger
            .session_offsets
            .as_ref()
            .ok_or_else(|| config_error("session merge offsets are missing from the ledger"))?;
        verify_destination(&resolved, session_offsets).await?;
        ledger.state = ConsolidationState::ArtifactsMerged;
        save_ledger(&ledger_path, &ledger)?;
        maybe_stop(&ledger.state, stop_after.as_ref())?;
    }

    if ledger.state == ConsolidationState::ArtifactsMerged {
        remove_verification_inputs(&resolved)?;
        register_destination(&resolved).await?;
        ledger.state = ConsolidationState::Registered;
        save_ledger(&ledger_path, &ledger)?;
        maybe_stop(&ledger.state, stop_after.as_ref())?;
    }

    if ledger.state == ConsolidationState::Registered {
        cut_over_markers(&resolved)?;
        ledger.state = ConsolidationState::Applied;
        save_ledger(&ledger_path, &ledger)?;
    }

    let mut report = resolved.report;
    report.state = ledger.state;
    report.dry_run = false;
    Ok(report)
}

fn maybe_stop(state: &ConsolidationState, stop_after: Option<&ConsolidationState>) -> Result<()> {
    if stop_after == Some(state) {
        return Err(config_error(format!(
            "synthetic interruption after {state:?}"
        )));
    }
    Ok(())
}

async fn resolve_plan(options: &ConsolidationOptions) -> Result<ResolvedPlan> {
    resolve_plan_inner(options, false, None).await
}

async fn resolve_plan_allowing_applied(options: &ConsolidationOptions) -> Result<ResolvedPlan> {
    resolve_plan_inner(options, true, None).await
}

async fn resolve_plan_inner(
    options: &ConsolidationOptions,
    allow_destination_marker: bool,
    evidence: Option<Arc<InputReadEvidence>>,
) -> Result<ResolvedPlan> {
    storage::validate_project_id(&options.source_project_id).map_err(config_error)?;
    storage::validate_project_id(&options.target_project_id).map_err(config_error)?;
    if options.source_project_id == options.target_project_id {
        return Err(config_error("source and target project ids must differ"));
    }
    let project_root = options
        .project_root
        .canonicalize()
        .map_err(|error| config_error(format!("could not resolve project root: {error}")))?;
    let profile_root = options
        .profile_root
        .canonicalize()
        .map_err(|error| config_error(format!("could not resolve profile root: {error}")))?;
    let git_common_dir = crate::worktree::git_common_dir(&project_root)
        .ok_or_else(|| config_error("project must be an attached git checkout"))?;
    let source_layout = layout_for_id(&project_root, &profile_root, &options.source_project_id)?;
    let target_layout = layout_for_id(&project_root, &profile_root, &options.target_project_id)?;
    let source_manifest = validate_manifest(&source_layout, &options.source_project_id)?;
    let target_manifest = validate_manifest(&target_layout, &options.target_project_id)?;
    let destination_project_id = destination_project_id(
        &git_common_dir,
        &options.source_project_id,
        &options.target_project_id,
    );
    let repository_marker = storage::read_repository_identity_marker(&project_root)?
        .ok_or_else(|| config_error("repository identity marker is required"))?;
    let marker_ok = repository_marker.project_id == options.target_project_id
        || (allow_destination_marker && repository_marker.project_id == destination_project_id);
    if !marker_ok {
        return Err(config_error(format!(
            "target project id '{}' is not the repository-selected shard '{}'",
            options.target_project_id, repository_marker.project_id
        )));
    }
    if !manifest_matches_identity(
        &source_manifest,
        &target_manifest,
        &project_root,
        &git_common_dir,
    ) || !manifest_matches_identity(
        &target_manifest,
        &target_manifest,
        &project_root,
        &git_common_dir,
    ) {
        return Err(config_error(
            "source and target manifests do not prove one exact git-common-dir identity",
        ));
    }
    reject_ambiguous_shards(
        options,
        &profile_root,
        &project_root,
        &git_common_dir,
        &target_manifest,
        &destination_project_id,
    )?;

    let source_meta = load_required_branch_meta(&source_layout)?;
    let target_meta = load_required_branch_meta(&target_layout)?;
    let source_graphs = graph_db_paths(&source_layout, &source_meta)?;
    let target_graphs = graph_db_paths(&target_layout, &target_meta)?;
    let session_paths = vec![
        source_layout.sessions_db_path.clone(),
        target_layout.sessions_db_path.clone(),
    ];
    let mut input_paths = source_graphs.clone();
    input_paths.extend(target_graphs.iter().cloned());
    input_paths.extend(session_paths.iter().cloned());
    input_paths.sort();
    input_paths.dedup();
    preflight::ensure_no_open_store_holders(&input_paths)?;
    let scratch_root = migration_scratch_root(&profile_root)?;
    let evidence = match evidence {
        Some(evidence) => {
            evidence.validate_content(&source_graphs, &target_graphs, &session_paths)?;
            evidence
        }
        None => Arc::new(
            capture_input_evidence(
                &source_graphs,
                &target_graphs,
                &session_paths,
                scratch_root.path(),
            )
            .await?,
        ),
    };
    let source = inventory_store(
        &evidence.source_graph,
        &evidence.sessions,
        &source_layout,
        &source_meta,
    )
    .await?;
    let target = inventory_store(
        &evidence.target_graph,
        &evidence.sessions,
        &target_layout,
        &target_meta,
    )
    .await?;
    let collisions = collision_summary(&evidence, &source_layout, &target_layout).await?;
    let input_fingerprint = fingerprint_inputs(&evidence, &source_layout, &target_layout)?;
    let migration_id = format!("consolidate_{}", &destination_project_id[5..]);
    let confirmation_token = confirmation_token(&input_fingerprint, &migration_id);
    let destination_data_root =
        storage::profile_sharded_data_root(&profile_root, &destination_project_id);
    let backup_root = profile_root.join(BACKUP_DIR).join(&migration_id);
    let ledger_path = profile_root
        .join(LEDGER_DIR)
        .join(format!("{migration_id}.json"));
    let state = load_ledger(&ledger_path)?
        .map(|ledger| ledger.state)
        .unwrap_or(ConsolidationState::Planned);
    Ok(ResolvedPlan {
        report: ConsolidationReport {
            migration_id,
            state,
            project_root,
            git_common_dir,
            source,
            target,
            destination_project_id,
            destination_data_root,
            backup_root,
            ledger_path,
            confirmation_token,
            collisions,
            dry_run: true,
        },
        input_fingerprint,
        source_layout,
        target_layout,
        source_meta,
        target_meta,
        scratch_root,
        evidence,
    })
}

fn layout_for_id(
    project_root: &Path,
    profile_root: &Path,
    project_id: &str,
) -> Result<StoreLayout> {
    storage::profile_sharded_layout(
        project_root,
        profile_root,
        &EnrollmentMarker {
            project_id: project_id.to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
}

fn validate_manifest(layout: &StoreLayout, project_id: &str) -> Result<StoreManifest> {
    let path = layout
        .manifest_path
        .as_ref()
        .ok_or_else(|| config_error("profile shard has no store manifest path"))?;
    let manifest = storage::read_store_manifest(path)?;
    if manifest.project_id.as_deref() != Some(project_id)
        || manifest.schema_version != storage::STORE_MANIFEST_SCHEMA_VERSION
        || manifest.store_kind != StoreKind::CodeProject
        || manifest.storage_mode != StorageMode::ProfileSharded
        || !same_path(&manifest.data_root, &layout.data_root)
        || !same_path(
            &manifest.data_root.join(&manifest.graph_db_relpath),
            &layout.graph_db_path,
        )
        || !same_path(
            &manifest.data_root.join(&manifest.sessions_db_relpath),
            &layout.sessions_db_path,
        )
        || !same_path(
            &manifest.data_root.join(&manifest.branch_meta_relpath),
            &layout.branch_meta_path,
        )
    {
        return Err(config_error(format!(
            "store manifest '{}' does not match profile shard '{}'",
            path.display(),
            layout.data_root.display()
        )));
    }
    Ok(manifest)
}

fn manifest_matches_identity(
    candidate: &StoreManifest,
    selected: &StoreManifest,
    project_root: &Path,
    git_common_dir: &Path,
) -> bool {
    if same_path(&candidate.project_root, project_root) {
        return true;
    }
    if candidate.project_root.is_dir()
        && crate::worktree::git_common_dir(&candidate.project_root)
            .is_some_and(|path| same_path(&path, git_common_dir))
    {
        return true;
    }
    // A repository move carries its identity marker inside the git common
    // directory. If both manifests name the same now-missing former root,
    // the marker-selected manifest is the proof that the pair moved together.
    !candidate.project_root.exists()
        && !selected.project_root.exists()
        && same_path(&candidate.project_root, &selected.project_root)
}

fn reject_ambiguous_shards(
    options: &ConsolidationOptions,
    profile_root: &Path,
    project_root: &Path,
    git_common_dir: &Path,
    selected: &StoreManifest,
    destination_project_id: &str,
) -> Result<()> {
    let projects = profile_root.join("projects");
    let mut matches = Vec::new();
    let Ok(entries) = fs::read_dir(projects) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path().join(storage::STORE_MANIFEST_FILENAME);
        let Ok(manifest) = storage::read_store_manifest(&path) else {
            continue;
        };
        let Some(project_id) = manifest.project_id.as_deref() else {
            continue;
        };
        if project_id == destination_project_id {
            continue;
        }
        if manifest_matches_identity(&manifest, selected, project_root, git_common_dir) {
            matches.push(project_id.to_string());
        }
    }
    matches.sort();
    matches.dedup();
    let expected = BTreeSet::from([
        options.source_project_id.clone(),
        options.target_project_id.clone(),
    ]);
    let actual = matches.iter().cloned().collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(config_error(format!(
            "ambiguous split-store identity: expected exactly {expected:?}, found {actual:?}; no files changed"
        )));
    }
    Ok(())
}

fn load_required_branch_meta(layout: &StoreLayout) -> Result<BranchMeta> {
    branch_meta::load_branch_meta(&layout.data_root).ok_or_else(|| {
        config_error(format!(
            "missing or invalid branch metadata at '{}'",
            layout.branch_meta_path.display()
        ))
    })
}

async fn inventory_store(
    graph: &GraphStoreEvidence,
    sessions: &crate::sqlite_read_snapshot::SnapshotSet,
    layout: &StoreLayout,
    meta: &BranchMeta,
) -> Result<StoreInventory> {
    let graph_paths = graph_db_paths(layout, meta)?;
    let (artifact_files, bytes) = tree_stats(&layout.data_root)?;
    Ok(StoreInventory {
        project_id: layout.identity.project_id.clone().unwrap_or_default(),
        data_root: layout.data_root.clone(),
        graph_databases: graph_paths.len(),
        facts: graph.identities.fact_count(),
        feedback_events: graph.identities.feedback_count(),
        sessions: sqlite::count_rows_in(sessions, &layout.sessions_db_path, "sessions").await?,
        messages: sqlite::count_rows_in(sessions, &layout.sessions_db_path, "session_messages")
            .await?,
        lcm_raw_messages: sqlite::count_rows_in(
            sessions,
            &layout.sessions_db_path,
            "lcm_raw_messages",
        )
        .await?,
        branches: meta.branches.len(),
        artifact_files,
        bytes,
    })
}

async fn collision_summary(
    evidence: &InputReadEvidence,
    source: &StoreLayout,
    target: &StoreLayout,
) -> Result<CollisionSummary> {
    let source_files = relative_file_map(&source.data_root)?;
    let target_files = relative_file_map(&target.data_root)?;
    let overlaps = source_files
        .keys()
        .filter(|path| target_files.contains_key(*path))
        .cloned()
        .collect::<Vec<_>>();
    let mut differing = Vec::new();
    for path in &overlaps {
        if is_runtime_lock(path) || is_sqlite_database(path) || is_sqlite_sidecar(path) {
            continue;
        }
        if file_digest(&source.data_root.join(path))? != file_digest(&target.data_root.join(path))?
        {
            differing.push(path.clone());
        }
    }
    let db = sqlite::inspect_collisions(
        &evidence.sessions,
        &source.sessions_db_path,
        &target.sessions_db_path,
    )
    .await?;
    Ok(CollisionSummary {
        fact_content_overlaps: evidence
            .source_graph
            .identities
            .fact_overlap(&evidence.target_graph.identities),
        session_overlaps: db.sessions,
        message_overlaps: db.messages,
        lcm_message_overlaps: db.lcm_messages,
        artifact_path_overlaps: overlaps.len(),
        differing_artifact_paths: differing,
        semantics: vec![
            "facts: union by content; tags/entities/metadata are merged, counters take maxima, newest trust/category wins, feedback events are deduplicated".to_string(),
            "sessions: union by provider/session id; time bounds widen and non-null target fields win".to_string(),
            "messages and LCM payload identities: identical rows deduplicate; divergent content is a hard error".to_string(),
            "branch graphs: target branches retain their names; every source branch is preserved under consolidated/<source-id>/...".to_string(),
            "artifact paths: identical files deduplicate; divergent non-reference files are preserved under consolidation-preserved; divergent payload/handle files are a hard error".to_string(),
        ],
    })
}

fn destination_project_id(git_common_dir: &Path, source: &str, target: &str) -> String {
    let mut ids = [source, target];
    ids.sort_unstable();
    let mut hash = Sha256::new();
    hash.update(b"tracedecay-profile-consolidation-v1\0");
    hash.update(
        canonical_or_original(git_common_dir)
            .to_string_lossy()
            .as_bytes(),
    );
    hash.update(b"\0");
    hash.update(ids[0].as_bytes());
    hash.update(b"\0");
    hash.update(ids[1].as_bytes());
    format!("proj_{}", &hex::encode(hash.finalize())[..16])
}

fn confirmation_token(fingerprint: &str, migration_id: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"tracedecay-consolidation-confirm-v1\0");
    hash.update(migration_id.as_bytes());
    hash.update(b"\0");
    hash.update(fingerprint.as_bytes());
    format!("confirm-{}", &hex::encode(hash.finalize())[..24])
}

fn fingerprint_inputs(
    evidence: &InputReadEvidence,
    source: &StoreLayout,
    target: &StoreLayout,
) -> Result<String> {
    let mut hash = Sha256::new();
    for (label, root, graph) in [
        ("source", &source.data_root, &evidence.source_graph),
        ("target", &target.data_root, &evidence.target_graph),
    ] {
        hash.update(label.as_bytes());
        for (relative, path) in relative_file_map(root)? {
            if is_runtime_lock(&relative) || is_sqlite_sidecar(&relative) {
                continue;
            }
            hash.update(relative.to_string_lossy().as_bytes());
            if is_sqlite_database(&relative) {
                let fingerprint = graph
                    .fingerprints
                    .get(&path)
                    .or_else(|| evidence.session_fingerprints.get(&path))
                    .ok_or_else(|| {
                        config_error(format!(
                            "missing logical fingerprint for '{}'",
                            path.display()
                        ))
                    })?;
                hash.update(fingerprint);
            } else {
                let fingerprint = file_digest(&path)?;
                hash.update(fingerprint);
            }
        }
    }
    Ok(hex::encode(hash.finalize()))
}

fn load_or_create_ledger(resolved: &ResolvedPlan, path: &Path) -> Result<ConsolidationLedger> {
    if let Some(ledger) = load_ledger(path)? {
        return Ok(ledger);
    }
    if resolved.report.destination_data_root.exists() {
        return Err(config_error(format!(
            "destination shard '{}' already exists without this migration ledger",
            resolved.report.destination_data_root.display()
        )));
    }
    let ledger = ConsolidationLedger {
        schema_version: LEDGER_SCHEMA_VERSION,
        migration_id: resolved.report.migration_id.clone(),
        confirmation_token: resolved.report.confirmation_token.clone(),
        input_fingerprint: resolved.input_fingerprint.clone(),
        source_project_id: resolved.report.source.project_id.clone(),
        target_project_id: resolved.report.target.project_id.clone(),
        destination_project_id: resolved.report.destination_project_id.clone(),
        project_root: resolved.report.project_root.clone(),
        git_common_dir: resolved.report.git_common_dir.clone(),
        state: ConsolidationState::Planned,
        graph_offsets: Vec::new(),
        session_offsets: None,
        preserved_collisions: Vec::new(),
    };
    save_ledger(path, &ledger)?;
    Ok(ledger)
}

fn validate_ledger(ledger: &ConsolidationLedger, resolved: &ResolvedPlan) -> Result<()> {
    if ledger.schema_version != LEDGER_SCHEMA_VERSION
        || ledger.migration_id != resolved.report.migration_id
        || ledger.confirmation_token != resolved.report.confirmation_token
        || ledger.input_fingerprint != resolved.input_fingerprint
        || ledger.source_project_id != resolved.report.source.project_id
        || ledger.target_project_id != resolved.report.target.project_id
        || ledger.destination_project_id != resolved.report.destination_project_id
        || !same_path(&ledger.git_common_dir, &resolved.report.git_common_dir)
    {
        return Err(config_error(
            "existing consolidation ledger does not match the current immutable input inventory",
        ));
    }
    Ok(())
}

fn load_ledger(path: &Path) -> Result<Option<ConsolidationLedger>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(error)),
    };
    serde_json::from_slice(&bytes).map(Some).map_err(|error| {
        config_error(format!(
            "consolidation ledger '{}' is corrupt: {error}",
            path.display()
        ))
    })
}

fn save_ledger(path: &Path, ledger: &ConsolidationLedger) -> Result<()> {
    let bytes =
        serde_json::to_vec_pretty(ledger).map_err(|error| config_error(error.to_string()))?;
    let temp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    PrivateStoreIo::write_file_atomically(path, &temp, &bytes).map_err(io_error)
}

fn backup_store(layout: &StoreLayout, backup_root: &Path) -> Result<()> {
    let project_id = layout.identity.project_id.as_deref().unwrap_or("unknown");
    copy_tree_exact(&layout.data_root, &backup_root.join(project_id))
}

async fn merge_databases(resolved: &ResolvedPlan, ledger: &mut ConsolidationLedger) -> Result<()> {
    let destination = &resolved.report.destination_data_root;
    let meta = load_required_branch_meta(&layout_for_id(
        &resolved.report.project_root,
        destination
            .parent()
            .and_then(Path::parent)
            .unwrap_or(Path::new("")),
        &resolved.report.destination_project_id,
    )?)?;
    let graph_paths = graph_db_paths_for_root(destination, &meta)?;
    if ledger.graph_offsets.is_empty() {
        ledger.graph_offsets = sqlite::plan_graph_offsets(&graph_paths).await?;
        save_ledger(&resolved.report.ledger_path, ledger)?;
    }
    sqlite::merge_graph_facts(&graph_paths, &ledger.graph_offsets).await?;

    let input_root = destination.join(INPUT_DIR);
    fs::create_dir_all(&input_root).map_err(io_error)?;
    let source_sessions = input_root.join("source-sessions.db");
    if !source_sessions.is_file() {
        copy_sqlite_family_exact(&resolved.source_layout.sessions_db_path, &source_sessions)?;
    }
    let target_sessions = destination.join(storage::SESSIONS_DB_FILENAME);
    if ledger.session_offsets.is_none() {
        ledger.session_offsets =
            Some(sqlite::plan_session_offsets(&target_sessions, &source_sessions).await?);
        save_ledger(&resolved.report.ledger_path, ledger)?;
    }
    let target_input = input_root.join("target-sessions.db");
    if !target_input.is_file() {
        copy_sqlite_family_exact(&target_sessions, &target_input)?;
    }
    let offsets = ledger
        .session_offsets
        .as_ref()
        .ok_or_else(|| config_error("session merge offsets are missing from the ledger"))?;
    sqlite::merge_sessions(&target_sessions, &source_sessions, offsets).await?;
    Ok(())
}

fn merge_non_database_artifacts(
    resolved: &ResolvedPlan,
    ledger: &mut ConsolidationLedger,
) -> Result<()> {
    let source = &resolved.source_layout.data_root;
    let destination = &resolved.report.destination_data_root;
    for (relative, path) in relative_file_map(source)? {
        if excluded_source_artifact(&relative) {
            continue;
        }
        let target = destination.join(&relative);
        if !target.exists() {
            copy_file_atomic(&path, &target)?;
            continue;
        }
        if file_digest(&path)? == file_digest(&target)? {
            continue;
        }
        if is_reference_artifact(&relative) {
            return Err(config_error(format!(
                "divergent referenced artifact collision at '{}'; both inputs and backups remain unchanged",
                relative.display()
            )));
        }
        let preserved = destination
            .join(PRESERVED_DIR)
            .join(&resolved.report.source.project_id)
            .join(&relative);
        copy_file_atomic(&path, &preserved)?;
        ledger.preserved_collisions.push(relative);
    }
    ledger.preserved_collisions.sort();
    ledger.preserved_collisions.dedup();
    Ok(())
}

fn remove_verification_inputs(resolved: &ResolvedPlan) -> Result<()> {
    let input = resolved.report.destination_data_root.join(INPUT_DIR);
    match fs::remove_dir_all(input) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(error)),
    }
}

fn write_destination_manifest(resolved: &ResolvedPlan) -> Result<()> {
    let layout = layout_for_id(
        &resolved.report.project_root,
        resolved
            .report
            .destination_data_root
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| config_error("destination shard has no profile root"))?,
        &resolved.report.destination_project_id,
    )?;
    storage::write_store_manifest(&layout).map(|_| ())
}

fn graph_db_paths(layout: &StoreLayout, meta: &BranchMeta) -> Result<Vec<PathBuf>> {
    graph_db_paths_for_root(&layout.data_root, meta)
}

fn input_database_paths(resolved: &ResolvedPlan) -> Result<Vec<PathBuf>> {
    database_paths_for_layouts(
        &resolved.source_layout,
        &resolved.source_meta,
        &resolved.target_layout,
        &resolved.target_meta,
    )
}

fn database_paths_for_layouts(
    source_layout: &StoreLayout,
    source_meta: &BranchMeta,
    target_layout: &StoreLayout,
    target_meta: &BranchMeta,
) -> Result<Vec<PathBuf>> {
    let mut paths = graph_db_paths(source_layout, source_meta)?;
    paths.extend(graph_db_paths(target_layout, target_meta)?);
    paths.push(source_layout.sessions_db_path.clone());
    paths.push(target_layout.sessions_db_path.clone());
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn graph_db_paths_for_root(root: &Path, meta: &BranchMeta) -> Result<Vec<PathBuf>> {
    let main = root.join(crate::config::DB_FILENAME);
    let mut paths = BTreeSet::new();
    for entry in meta.branches.values() {
        let path = root.join(&entry.db_file);
        if !path.is_file() {
            return Err(config_error(format!(
                "branch graph '{}' is missing",
                path.display()
            )));
        }
        paths.insert(path);
    }
    if !paths.remove(&main) {
        return Err(config_error(format!(
            "default graph '{}' is not present in branch metadata",
            main.display()
        )));
    }
    Ok(std::iter::once(main).chain(paths).collect())
}

fn same_path(left: &Path, right: &Path) -> bool {
    canonical_or_original(left) == canonical_or_original(right)
}

fn canonical_or_original(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn migration_scratch_root(profile_root: &Path) -> Result<MigrationScratchRoot> {
    let parent = profile_root
        .parent()
        .ok_or_else(|| config_error("profile root has no parent for private migration scratch"))?;
    let mut hash = Sha256::new();
    hash.update(profile_root.to_string_lossy().as_bytes());
    let prefix = format!(
        ".tracedecay-migration-scratch-{}-{}",
        &hex::encode(hash.finalize())[..12],
        std::process::id()
    );
    for _ in 0..100 {
        let sequence = NEXT_MIGRATION_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!("{prefix}-{sequence}"));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&path) {
            Ok(()) => return Ok(MigrationScratchRoot { path }),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io_error(error)),
        }
    }
    Err(config_error(
        "could not allocate a private migration scratch directory",
    ))
}

fn git_remote_url(project_root: &Path) -> Option<String> {
    let repo = gix::discover(project_root).ok()?;
    let value = repo.config_snapshot().string("remote.origin.url")?;
    let value = value.to_string();
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn io_error(error: io::Error) -> TraceDecayError {
    config_error(error.to_string())
}

#[cfg(test)]
mod tests;
