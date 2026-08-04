//! Explicit, offline consolidation of two profile shards for one repository.
//!
//! The runtime deliberately refuses to guess when both shards contain data.
//! This workflow builds a third deterministic shard, preserving both inputs
//! and cutting the repository marker over only after the new shard and global
//! registry have verified successfully.

#[doc(hidden)]
pub mod evidence;
#[doc(hidden)]
pub mod files;
mod finalize;
mod preflight;
#[doc(hidden)]
pub mod prepare;
#[doc(hidden)]
pub mod sqlite;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use libsql::params;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use evidence::{GraphStoreEvidence, InputReadEvidence, capture_input_evidence};
#[doc(hidden)]
pub use files::{
    copy_file_atomic, copy_file_exact, copy_sqlite_family_exact, file_digest, relative_file_map,
    sqlite_sidecar,
};
use files::{
    excluded_source_artifact, is_coordination_lock, is_reference_artifact, is_runtime_lock,
    is_sqlite_database, is_sqlite_sidecar, tree_stats,
};
use finalize::{cut_over_markers, register_destination, verify_destination};
use preflight::{acquire_store_locks, ensure_profile_offline, preflight_disk_space};
use prepare::prepare_destination;

use crate::branch_meta::{self, BranchEntry, BranchMeta};
use crate::errors::{Result, TraceDecayError};
use crate::registry_adapter::{RegistryDatabase, RegistryRuntime, canonical_project_key};
use crate::storage::{
    self, EnrollmentMarker, PrivateStoreIo, StorageMode, StoreKind, StoreLayout, StoreManifest,
};

#[doc(hidden)]
pub const LEDGER_SCHEMA_VERSION: u32 = 2;
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
    #[serde(default)]
    pub divergent_lcm_messages: u64,
    #[serde(default)]
    pub divergent_lcm_session_ids: u64,
    #[serde(default)]
    pub divergent_lcm_content_hashes: u64,
    #[serde(default)]
    pub divergent_lcm_storage_kinds: u64,
    #[serde(default)]
    pub divergent_lcm_payload_refs: u64,
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
#[doc(hidden)]
pub struct ConsolidationLedger {
    pub schema_version: u32,
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

#[derive(Debug, Default)]
pub struct ManifestRetirementReport {
    pub retired: Vec<PathBuf>,
    pub retired_registry_projects: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManifestRetirementAction {
    RenameCanonical,
    RemoveDuplicateCanonical,
    AlreadyRetired,
}

struct ManifestRetirementPlan {
    canonical_path: PathBuf,
    retired_path: PathBuf,
    manifest: StoreManifest,
    action: ManifestRetirementAction,
}

#[doc(hidden)]
pub struct ResolvedPlan {
    pub report: ConsolidationReport,
    input_fingerprint: String,
    pub source_layout: StoreLayout,
    pub target_layout: StoreLayout,
    source_meta: BranchMeta,
    target_meta: BranchMeta,
    pub evidence: Arc<InputReadEvidence>,
    scratch_root: MigrationScratchRoot,
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

pub async fn plan_with_daemon_status(
    options: &ConsolidationOptions,
    daemon_reachable: bool,
) -> Result<ConsolidationReport> {
    ensure_profile_offline(options, daemon_reachable)?;
    let lifecycle = crate::lifecycle_lease::acquire_exclusive_for_profile(
        &options.profile_root,
        "profile shard consolidation plan",
    )?;
    let _database_scope = crate::db::enter_maintenance_database_scope(
        &lifecycle,
        &options.profile_root,
        "profile shard consolidation plan",
    )?;
    Ok(resolve_plan(options).await?.report)
}

pub async fn apply_with_registry<R: RegistryRuntime>(
    options: &ConsolidationOptions,
    confirmation_token: &str,
    daemon_reachable: bool,
    registry: &R,
) -> Result<ConsolidationReport> {
    apply_with_stop(
        options,
        confirmation_token,
        None,
        daemon_reachable,
        registry,
    )
    .await
}

#[doc(hidden)]
pub async fn apply_with_stop<R: RegistryRuntime>(
    options: &ConsolidationOptions,
    confirmation_token: &str,
    stop_after: Option<ConsolidationState>,
    daemon_reachable: bool,
    registry: &R,
) -> Result<ConsolidationReport> {
    apply_with_faults(
        options,
        confirmation_token,
        stop_after,
        None,
        daemon_reachable,
        registry,
    )
    .await
}

#[doc(hidden)]
pub async fn apply_with_prepare_stop<R: RegistryRuntime>(
    options: &ConsolidationOptions,
    confirmation_token: &str,
    prepare_stop: prepare::PrepareStop,
    daemon_reachable: bool,
    registry: &R,
) -> Result<ConsolidationReport> {
    apply_with_faults(
        options,
        confirmation_token,
        None,
        Some(prepare_stop),
        daemon_reachable,
        registry,
    )
    .await
}

async fn apply_with_faults<R: RegistryRuntime>(
    options: &ConsolidationOptions,
    confirmation_token: &str,
    stop_after: Option<ConsolidationState>,
    prepare_stop: Option<prepare::PrepareStop>,
    daemon_reachable: bool,
    registry: &R,
) -> Result<ConsolidationReport> {
    ensure_profile_offline(options, daemon_reachable)?;
    let lifecycle = crate::lifecycle_lease::acquire_exclusive_for_profile(
        &options.profile_root,
        "profile shard consolidation",
    )?;
    let _database_scope = crate::db::enter_maintenance_database_scope(
        &lifecycle,
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
        finalize_applied_consolidation(&options.profile_root, &ledger, registry).await?;
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
        merge_databases(&resolved, &mut ledger, registry).await?;
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
        register_destination(&resolved, registry).await?;
        ledger.state = ConsolidationState::Registered;
        save_ledger(&ledger_path, &ledger)?;
        maybe_stop(&ledger.state, stop_after.as_ref())?;
    }

    if ledger.state == ConsolidationState::Registered {
        cut_over_markers(&resolved)?;
        ledger.state = ConsolidationState::Applied;
        save_ledger(&ledger_path, &ledger)?;
    }

    if ledger.state == ConsolidationState::Applied {
        finalize_applied_consolidation(&options.profile_root, &ledger, registry).await?;
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

#[doc(hidden)]
pub async fn resolve_plan(options: &ConsolidationOptions) -> Result<ResolvedPlan> {
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
    let destination_project_id = destination_project_id(
        &git_common_dir,
        &options.source_project_id,
        &options.target_project_id,
    );
    let migration_id = format!("consolidate_{}", &destination_project_id[5..]);
    let ledger_path = profile_root
        .join(LEDGER_DIR)
        .join(format!("{migration_id}.json"));
    let applied_ledger = load_ledger(&ledger_path)?.filter(|ledger| {
        ledger.schema_version == LEDGER_SCHEMA_VERSION
            && ledger.state == ConsolidationState::Applied
    });
    let allow_retired_manifests = allow_destination_marker && applied_ledger.is_some();
    let source_layout = layout_for_id(&project_root, &profile_root, &options.source_project_id)?;
    let target_layout = layout_for_id(&project_root, &profile_root, &options.target_project_id)?;
    let source_manifest = validate_input_manifest(
        &source_layout,
        &options.source_project_id,
        &destination_project_id,
        allow_retired_manifests,
    )?;
    let target_manifest = validate_input_manifest(
        &target_layout,
        &options.target_project_id,
        &destination_project_id,
        allow_retired_manifests,
    );
    let target_manifest = target_manifest?;
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
    if applied_ledger.is_none() {
        reject_ambiguous_shards(
            options,
            &profile_root,
            &project_root,
            &git_common_dir,
            &target_manifest,
            &destination_project_id,
        )?;
    }

    let mut source_meta = load_input_branch_meta(&source_layout)?;
    let mut target_meta = load_input_branch_meta(&target_layout)?;
    recover_untracked_branch_graphs(&source_layout, &mut source_meta)?;
    recover_untracked_branch_graphs(&target_layout, &mut target_meta)?;
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
    let input_fingerprint = fingerprint_inputs(
        &evidence,
        &source_layout,
        &source_meta,
        &target_layout,
        &target_meta,
        applied_ledger
            .as_ref()
            .map(|_| destination_project_id.as_str()),
    )?;
    let confirmation_token = confirmation_token(&input_fingerprint, &migration_id);
    let destination_data_root =
        storage::profile_sharded_data_root(&profile_root, &destination_project_id);
    let backup_root = profile_root.join(BACKUP_DIR).join(&migration_id);
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

#[doc(hidden)]
pub fn layout_for_id(
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
    validate_manifest_path(path, layout, project_id)
}

fn validate_input_manifest(
    layout: &StoreLayout,
    project_id: &str,
    destination_project_id: &str,
    allow_retired: bool,
) -> Result<StoreManifest> {
    if !allow_retired {
        return validate_manifest(layout, project_id);
    }
    Ok(inspect_manifest_retirement(layout, project_id, destination_project_id)?.manifest)
}

fn validate_manifest_path(
    path: &Path,
    layout: &StoreLayout,
    project_id: &str,
) -> Result<StoreManifest> {
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
        || !manifest_branch_meta_path_matches_layout(&manifest, layout)
    {
        return Err(config_error(format!(
            "store manifest '{}' does not match profile shard '{}'",
            path.display(),
            layout.data_root.display()
        )));
    }
    Ok(manifest)
}

fn manifest_branch_meta_path_matches_layout(
    manifest: &StoreManifest,
    layout: &StoreLayout,
) -> bool {
    layout
        .branch_meta_path
        .strip_prefix(&layout.data_root)
        .is_ok_and(|relative| manifest.branch_meta_relpath == relative)
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

fn load_input_branch_meta(layout: &StoreLayout) -> Result<BranchMeta> {
    match fs::symlink_metadata(&layout.branch_meta_path) {
        Ok(metadata) if !metadata.file_type().is_file() => Err(config_error(format!(
            "corrupt branch metadata at '{}': path is not a regular file",
            layout.branch_meta_path.display()
        ))),
        Ok(_) => {
            let content = fs::read_to_string(&layout.branch_meta_path).map_err(|error| {
                config_error(format!(
                    "could not read branch metadata at '{}': {error}",
                    layout.branch_meta_path.display()
                ))
            })?;
            branch_meta::parse(&content).map_err(|error| {
                config_error(format!(
                    "corrupt branch metadata at '{}': {error}",
                    layout.branch_meta_path.display()
                ))
            })
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if !layout.graph_db_path.is_file() {
                return Err(config_error(format!(
                    "missing branch metadata at '{}' and default graph database at '{}'",
                    layout.branch_meta_path.display(),
                    layout.graph_db_path.display()
                )));
            }
            let default_branch = crate::branch::detect_default_branch(&layout.project_root)
                .ok_or_else(|| {
                    config_error(format!(
                        "cannot synthesize missing branch metadata at '{}': repository default branch is unknown (detached HEAD or no default ref)",
                        layout.branch_meta_path.display()
                    ))
                })?;
            Ok(BranchMeta::for_legacy_single_db(
                &layout.data_root,
                &default_branch,
            ))
        }
        Err(error) => Err(config_error(format!(
            "could not inspect branch metadata at '{}': {error}",
            layout.branch_meta_path.display()
        ))),
    }
}

fn load_required_branch_meta(layout: &StoreLayout) -> Result<BranchMeta> {
    branch_meta::load_branch_meta(&layout.data_root).ok_or_else(|| {
        config_error(format!(
            "missing or invalid branch metadata at '{}'",
            layout.branch_meta_path.display()
        ))
    })
}

fn recover_untracked_branch_graphs(layout: &StoreLayout, meta: &mut BranchMeta) -> Result<()> {
    for (relative, path) in relative_file_map(&layout.data_root)? {
        if !relative.starts_with("branches") || !is_sqlite_database(&relative) {
            continue;
        }
        if meta
            .branches
            .values()
            .any(|entry| same_path(&layout.data_root.join(&entry.db_file), &path))
        {
            continue;
        }
        let db_file = relative.to_str().ok_or_else(|| {
            config_error(format!(
                "untracked branch graph '{}' cannot be represented in branch metadata",
                path.display()
            ))
        })?;
        let db_file = db_file.replace('\\', "/");
        let base = relative
            .file_stem()
            .and_then(|value| value.to_str())
            .map(crate::branch::sanitize_branch_name)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "branch".to_string());
        let mut hash = Sha256::new();
        hash.update(relative.as_os_str().as_encoded_bytes());
        let name = format!("recovered/{base}-{}", &hex::encode(hash.finalize())[..16]);
        if meta.branches.contains_key(&name) {
            return Err(config_error(format!(
                "recovered branch name collision for '{}'",
                path.display()
            )));
        }
        meta.branches.insert(
            name,
            BranchEntry {
                db_file,
                parent: Some(meta.default_branch.clone()),
                created_at: "0".to_string(),
                last_synced_at: "0".to_string(),
                gc_protected: true,
            },
        );
    }
    Ok(())
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
        divergent_lcm_messages: db.divergent_lcm_messages,
        divergent_lcm_session_ids: db.divergent_lcm_session_ids,
        divergent_lcm_content_hashes: db.divergent_lcm_content_hashes,
        divergent_lcm_storage_kinds: db.divergent_lcm_storage_kinds,
        divergent_lcm_payload_refs: db.divergent_lcm_payload_refs,
        artifact_path_overlaps: overlaps.len(),
        differing_artifact_paths: differing,
        semantics: vec![
            "facts: union by content; tags/entities/metadata are merged, counters take maxima, newest trust/category wins, feedback events are deduplicated".to_string(),
            "sessions: union by provider/session id; time bounds widen and non-null target fields win".to_string(),
            "session-message projections: the selected target row remains canonical; divergent source rows and their overlapping parent-linked session family are preserved as active consolidated/<source-project-id>/<message-id> variants".to_string(),
            "LCM raw messages: the selected target row remains canonical; source rows receive active variants only for content-hash divergence, while equal-content representation drift deduplicates to the target raw family".to_string(),
            "LCM external payloads and summaries: identical identities deduplicate; divergent content is a hard error".to_string(),
            "branch graphs: target branches retain their names; every source branch is preserved under consolidated/<source-id>/...".to_string(),
            "artifact paths: identical files deduplicate; divergent non-reference files are preserved under consolidation-preserved; divergent payload/handle files are a hard error".to_string(),
        ],
    })
}

pub fn destination_project_id(git_common_dir: &Path, source: &str, target: &str) -> String {
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
    source_meta: &BranchMeta,
    target: &StoreLayout,
    target_meta: &BranchMeta,
    retired_destination_project_id: Option<&str>,
) -> Result<String> {
    let mut hash = Sha256::new();
    for (label, root, graph, meta) in [
        (
            "source",
            &source.data_root,
            &evidence.source_graph,
            source_meta,
        ),
        (
            "target",
            &target.data_root,
            &evidence.target_graph,
            target_meta,
        ),
    ] {
        hash.update(label.as_bytes());
        hash.update(b"\0branch-meta\0");
        hash.update(serde_json::to_vec(meta).map_err(|error| {
            config_error(format!(
                "could not canonicalize {label} branch metadata for input fingerprint: {error}"
            ))
        })?);
        let mut files = BTreeMap::new();
        for (relative, path) in relative_file_map(root)? {
            let retired_manifest_name = retired_destination_project_id
                .map(|destination| format!("store_manifest.consolidated-into-{destination}.json"));
            let relative = retired_destination_project_id
                .filter(|_| {
                    retired_manifest_name
                        .as_deref()
                        .is_some_and(|name| relative == Path::new(name))
                })
                .map(|_| PathBuf::from(storage::STORE_MANIFEST_FILENAME))
                .unwrap_or(relative);
            if let Some(existing) = files.insert(relative.clone(), path.clone())
                && file_digest(&existing)? != file_digest(&path)?
            {
                return Err(config_error(format!(
                    "duplicate normalized consolidation input '{}' diverges",
                    relative.display()
                )));
            }
        }
        for (relative, path) in files {
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
    if let Some(mut ledger) = load_ledger(path)? {
        validate_ledger_inventory(&ledger, resolved)?;
        if ledger.schema_version == 1 {
            if !matches!(
                ledger.state,
                ConsolidationState::Planned
                    | ConsolidationState::BackupsReady
                    | ConsolidationState::DestinationReady
            ) {
                return Err(config_error(format!(
                    "consolidation ledger v1 cannot be migrated safely after database merge state {:?}",
                    ledger.state
                )));
            }
            ledger.schema_version = LEDGER_SCHEMA_VERSION;
            save_ledger(path, &ledger)?;
        }
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
    validate_ledger_inventory(ledger, resolved)?;
    if ledger.schema_version != LEDGER_SCHEMA_VERSION {
        return Err(config_error(format!(
            "unsupported consolidation ledger schema version {}; expected {}",
            ledger.schema_version, LEDGER_SCHEMA_VERSION
        )));
    }
    Ok(())
}

fn validate_ledger_inventory(ledger: &ConsolidationLedger, resolved: &ResolvedPlan) -> Result<()> {
    if ledger.migration_id != resolved.report.migration_id
        || ledger.confirmation_token != resolved.report.confirmation_token
        || ledger.input_fingerprint != resolved.input_fingerprint
        || ledger.source_project_id != resolved.report.source.project_id
        || ledger.target_project_id != resolved.report.target.project_id
        || ledger.destination_project_id != resolved.report.destination_project_id
        || !same_path(&ledger.project_root, &resolved.report.project_root)
        || !same_path(&ledger.git_common_dir, &resolved.report.git_common_dir)
    {
        return Err(config_error(
            "existing consolidation ledger does not match the current immutable input inventory",
        ));
    }
    Ok(())
}

#[doc(hidden)]
pub fn load_ledger(path: &Path) -> Result<Option<ConsolidationLedger>> {
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

#[doc(hidden)]
pub fn save_ledger(path: &Path, ledger: &ConsolidationLedger) -> Result<()> {
    let bytes =
        serde_json::to_vec_pretty(ledger).map_err(|error| config_error(error.to_string()))?;
    let temp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    PrivateStoreIo::write_file_atomically(path, &temp, &bytes).map_err(io_error)
}

pub async fn retire_applied_input_manifests_with_registry<R: RegistryRuntime>(
    profile_root: &Path,
    registry: &R,
) -> ManifestRetirementReport {
    let mut report = ManifestRetirementReport::default();
    let ledger_root = profile_root.join(LEDGER_DIR);
    let entries = match fs::read_dir(&ledger_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return report,
        Err(error) => {
            report.warnings.push(format!(
                "could not read consolidation ledger directory '{}': {error}",
                ledger_root.display()
            ));
            return report;
        }
    };
    let mut ledger_paths = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("consolidate_"))
                && path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
        })
        .collect::<Vec<_>>();
    ledger_paths.sort();

    for ledger_path in ledger_paths {
        let ledger = match load_ledger(&ledger_path) {
            Ok(Some(ledger)) => ledger,
            Ok(None) => continue,
            Err(error) => {
                report.warnings.push(error.to_string());
                continue;
            }
        };
        if ledger.state != ConsolidationState::Applied {
            continue;
        }
        if ledger.schema_version != LEDGER_SCHEMA_VERSION {
            report.warnings.push(format!(
                "applied consolidation ledger '{}' uses schema version {}; expected {}",
                ledger_path.display(),
                ledger.schema_version,
                LEDGER_SCHEMA_VERSION
            ));
            continue;
        }
        match finalize_applied_consolidation(profile_root, &ledger, registry).await {
            Ok((retired, registry_projects)) => {
                report.retired.extend(retired);
                report.retired_registry_projects = report
                    .retired_registry_projects
                    .saturating_add(registry_projects);
            }
            Err(error) => report.warnings.push(error.to_string()),
        }
    }
    report
}

async fn finalize_applied_consolidation<R: RegistryRuntime>(
    profile_root: &Path,
    ledger: &ConsolidationLedger,
    registry: &R,
) -> Result<(Vec<PathBuf>, usize)> {
    validate_applied_retirement_authority(profile_root, ledger)?;
    let source_layout = layout_for_id(
        &ledger.project_root,
        profile_root,
        &ledger.source_project_id,
    )?;
    let target_layout = layout_for_id(
        &ledger.project_root,
        profile_root,
        &ledger.target_project_id,
    )?;
    let plans = [
        inspect_manifest_retirement(
            &source_layout,
            &ledger.source_project_id,
            &ledger.destination_project_id,
        )?,
        inspect_manifest_retirement(
            &target_layout,
            &ledger.target_project_id,
            &ledger.destination_project_id,
        )?,
    ];
    let mut retired = Vec::new();
    for plan in plans {
        let parent = plan
            .canonical_path
            .parent()
            .ok_or_else(|| config_error("input store manifest has no parent directory"))?;
        match plan.action {
            ManifestRetirementAction::RenameCanonical => {
                fs::rename(&plan.canonical_path, &plan.retired_path).map_err(io_error)?;
                files::sync_parent_directory(parent)?;
                retired.push(plan.retired_path);
            }
            ManifestRetirementAction::RemoveDuplicateCanonical => {
                fs::remove_file(&plan.canonical_path).map_err(io_error)?;
                files::sync_parent_directory(parent)?;
                retired.push(plan.retired_path);
            }
            ManifestRetirementAction::AlreadyRetired => {}
        }
    }
    let retired_registry_projects =
        retire_legacy_registry_owners(profile_root, ledger, registry).await?;
    Ok((retired, retired_registry_projects))
}

fn validate_applied_retirement_authority(
    profile_root: &Path,
    ledger: &ConsolidationLedger,
) -> Result<()> {
    if ledger.schema_version != LEDGER_SCHEMA_VERSION || ledger.state != ConsolidationState::Applied
    {
        return Err(config_error(
            "source manifest retirement requires an applied schema-2 consolidation ledger",
        ));
    }
    for project_id in [
        &ledger.source_project_id,
        &ledger.target_project_id,
        &ledger.destination_project_id,
    ] {
        storage::validate_project_id(project_id).map_err(config_error)?;
    }
    let expected_destination = destination_project_id(
        &ledger.git_common_dir,
        &ledger.source_project_id,
        &ledger.target_project_id,
    );
    let expected_migration = format!("consolidate_{}", &expected_destination[5..]);
    if ledger.destination_project_id != expected_destination
        || ledger.migration_id != expected_migration
    {
        return Err(config_error(
            "applied consolidation ledger identity does not match its deterministic destination",
        ));
    }

    let repository = storage::read_repository_identity_marker(&ledger.project_root)?
        .ok_or_else(|| config_error("repository identity marker is missing after consolidation"))?;
    let enrollment = storage::read_enrollment_marker(&ledger.project_root)?
        .ok_or_else(|| config_error("enrollment marker is missing after consolidation"))?;
    if repository.project_id != ledger.destination_project_id
        || !same_path(
            Path::new(&repository.git_common_dir),
            &ledger.git_common_dir,
        )
        || enrollment.project_id != ledger.destination_project_id
        || enrollment.storage_mode != StorageMode::ProfileSharded
    {
        return Err(config_error(format!(
            "consolidation marker mismatch for destination project '{}'",
            ledger.destination_project_id
        )));
    }

    let destination_layout = layout_for_id(
        &ledger.project_root,
        profile_root,
        &ledger.destination_project_id,
    )?;
    validate_manifest(&destination_layout, &ledger.destination_project_id)?;
    Ok(())
}

async fn retire_legacy_registry_owners<R: RegistryRuntime>(
    profile_root: &Path,
    ledger: &ConsolidationLedger,
    registry: &R,
) -> Result<usize> {
    let global_path = profile_root.join("global.db");
    if !global_path.is_file() {
        return Err(config_error(format!(
            "global registry '{}' is missing for applied consolidation",
            global_path.display()
        )));
    }
    let db = registry
        .open_at(&global_path)
        .await
        .ok_or_else(|| config_error("could not open global registry for consolidation cleanup"))?;
    let conn = db.conn();
    conn.execute("BEGIN IMMEDIATE", ())
        .await
        .map_err(|error| config_error(format!("could not begin registry cleanup: {error}")))?;

    let injected_failure = registry.fail_registry_retirement_once(profile_root) || {
        #[cfg(test)]
        {
            let injected_failure = profile_root
                .join(LEDGER_DIR)
                .join(".fail-registry-retirement-once");
            if injected_failure.is_file() {
                let _ = fs::remove_file(injected_failure);
                true
            } else {
                false
            }
        }
        #[cfg(not(test))]
        {
            false
        }
    };
    if injected_failure {
        let _ = conn.execute("ROLLBACK", ()).await;
        return Err(config_error(
            "synthetic registry retirement failure after manifest retirement",
        ));
    }

    let result = async {
        let canonical_root = canonical_project_key(&ledger.project_root);
        let mut rows = conn
            .query(
                "SELECT canonical_root, COALESCE(git_common_dir, '')
                 FROM code_projects WHERE project_id=?1",
                params![ledger.destination_project_id.as_str()],
            )
            .await
            .map_err(|error| {
                config_error(format!("could not validate destination project: {error}"))
            })?;
        let destination = rows
            .next()
            .await
            .map_err(|error| config_error(format!("could not read destination project: {error}")))?
            .ok_or_else(|| config_error("destination registry project is missing"))?;
        let registered_root = destination.get::<String>(0).map_err(|error| {
            config_error(format!("invalid destination canonical root: {error}"))
        })?;
        let registered_common = destination.get::<String>(1).map_err(|error| {
            config_error(format!("invalid destination git common dir: {error}"))
        })?;
        if registered_root != canonical_root
            || registered_common.is_empty()
            || !same_path(Path::new(&registered_common), &ledger.git_common_dir)
        {
            return Err(config_error(
                "destination registry project does not match the applied consolidation ledger",
            ));
        }

        let mut rows = conn
            .query(
                "SELECT project_id FROM project_aliases WHERE alias_path=?1",
                params![canonical_root.as_str()],
            )
            .await
            .map_err(|error| {
                config_error(format!("could not validate destination alias: {error}"))
            })?;
        let alias_owner = rows
            .next()
            .await
            .map_err(|error| config_error(format!("could not read destination alias: {error}")))?
            .and_then(|row| row.get::<String>(0).ok());
        if alias_owner.as_deref() != Some(ledger.destination_project_id.as_str()) {
            return Err(config_error(
                "destination registry alias does not match the applied consolidation ledger",
            ));
        }

        let store_id = format!("store:{}:profile_sharded", ledger.destination_project_id);
        let store_relpath = format!("projects/{}", ledger.destination_project_id);
        let manifest_relpath = format!("{store_relpath}/{}", storage::STORE_MANIFEST_FILENAME);
        let mut rows = conn
            .query(
                "SELECT project_id, store_kind, storage_mode, store_relpath,
                        COALESCE(manifest_relpath, '')
                 FROM store_instances WHERE store_id=?1",
                params![store_id.as_str()],
            )
            .await
            .map_err(|error| {
                config_error(format!("could not validate destination store: {error}"))
            })?;
        let store = rows
            .next()
            .await
            .map_err(|error| config_error(format!("could not read destination store: {error}")))?
            .ok_or_else(|| config_error("destination registry store is missing"))?;
        let store_values = (0..5)
            .map(|index| {
                store.get::<String>(index).map_err(|error| {
                    config_error(format!("invalid destination store registry row: {error}"))
                })
            })
            .collect::<Result<Vec<_>>>()?;
        if store_values
            != vec![
                ledger.destination_project_id.clone(),
                "code_project".to_string(),
                "profile_sharded".to_string(),
                store_relpath,
                manifest_relpath,
            ]
        {
            return Err(config_error(
                "destination registry store does not match the applied consolidation ledger",
            ));
        }

        let canonical_common = canonical_project_key(&ledger.git_common_dir);
        let mut rows = conn
            .query(
                "SELECT project_id FROM code_projects WHERE canonical_root=?1 ORDER BY project_id",
                params![canonical_root.as_str()],
            )
            .await
            .map_err(|error| {
                config_error(format!("could not validate canonical owners: {error}"))
            })?;
        let mut owners = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| config_error(format!("could not read canonical owner: {error}")))?
        {
            owners.push(row.get::<String>(0).map_err(|error| {
                config_error(format!("invalid canonical owner registry row: {error}"))
            })?);
        }
        let allowed = BTreeSet::from([
            ledger.source_project_id.clone(),
            ledger.target_project_id.clone(),
            ledger.destination_project_id.clone(),
        ]);
        if owners.iter().any(|owner| !allowed.contains(owner))
            || !owners.contains(&ledger.destination_project_id)
        {
            return Err(config_error(format!(
                "canonical root has unexpected registry owners: {owners:?}"
            )));
        }

        // Old project IDs can have been rebound to another repository. Match
        // the full consolidation identity so those moved rows survive.
        let deleted = conn
            .execute(
                "DELETE FROM code_projects
                 WHERE project_id IN (?1, ?2)
                   AND canonical_root=?3
                   AND git_common_dir=?4",
                params![
                    ledger.source_project_id.as_str(),
                    ledger.target_project_id.as_str(),
                    canonical_root.as_str(),
                    canonical_common.as_str()
                ],
            )
            .await
            .map_err(|error| {
                config_error(format!("could not retire legacy registry owners: {error}"))
            })?;

        let mut rows = conn
            .query(
                "SELECT project_id FROM code_projects WHERE canonical_root=?1 ORDER BY project_id",
                params![canonical_root.as_str()],
            )
            .await
            .map_err(|error| {
                config_error(format!("could not verify canonical owner cleanup: {error}"))
            })?;
        let remaining = rows
            .next()
            .await
            .map_err(|error| {
                config_error(format!("could not read remaining canonical owner: {error}"))
            })?
            .and_then(|row| row.get::<String>(0).ok());
        let extra = rows.next().await.map_err(|error| {
            config_error(format!("could not read extra canonical owner: {error}"))
        })?;
        if remaining.as_deref() != Some(ledger.destination_project_id.as_str()) || extra.is_some() {
            return Err(config_error(
                "registry cleanup did not leave exactly one destination canonical owner",
            ));
        }
        usize::try_from(deleted)
            .map_err(|_| config_error("legacy registry cleanup count overflowed usize"))
    }
    .await;

    match result {
        Ok(deleted) => match conn.execute("COMMIT", ()).await {
            Ok(_) => Ok(deleted),
            Err(error) => {
                let _ = conn.execute("ROLLBACK", ()).await;
                Err(config_error(format!(
                    "could not commit legacy registry cleanup: {error}"
                )))
            }
        },
        Err(error) => {
            let _ = conn.execute("ROLLBACK", ()).await;
            Err(error)
        }
    }
}

fn inspect_manifest_retirement(
    layout: &StoreLayout,
    project_id: &str,
    destination_project_id: &str,
) -> Result<ManifestRetirementPlan> {
    let canonical_path = layout
        .manifest_path
        .clone()
        .ok_or_else(|| config_error("profile shard has no store manifest path"))?;
    let retired_path = canonical_path.with_file_name(format!(
        "store_manifest.consolidated-into-{destination_project_id}.json"
    ));
    let canonical = read_optional_regular_file(&canonical_path)?;
    let retired = read_optional_regular_file(&retired_path)?;
    let (path, action) = match (&canonical, &retired) {
        (Some(_), None) => (
            canonical_path.as_path(),
            ManifestRetirementAction::RenameCanonical,
        ),
        (None, Some(_)) => (
            retired_path.as_path(),
            ManifestRetirementAction::AlreadyRetired,
        ),
        (Some(canonical), Some(retired)) if canonical == retired => (
            retired_path.as_path(),
            ManifestRetirementAction::RemoveDuplicateCanonical,
        ),
        (Some(_), Some(_)) => {
            return Err(config_error(format!(
                "canonical and retired store manifests diverge for project '{project_id}'"
            )));
        }
        (None, None) => {
            return Err(config_error(format!(
                "neither canonical nor retired store manifest exists for project '{project_id}'"
            )));
        }
    };
    let manifest = validate_manifest_path(path, layout, project_id)?;
    Ok(ManifestRetirementPlan {
        canonical_path,
        retired_path,
        manifest,
        action,
    })
}

fn read_optional_regular_file(path: &Path) -> Result<Option<Vec<u8>>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(error)),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(config_error(format!(
            "store manifest '{}' is not a regular file",
            path.display()
        )));
    }
    fs::read(path).map(Some).map_err(io_error)
}

fn backup_store(layout: &StoreLayout, backup_root: &Path) -> Result<()> {
    let project_id = layout.identity.project_id.as_deref().unwrap_or("unknown");
    let destination = backup_root.join(project_id);
    for (relative, path) in relative_file_map(&layout.data_root)? {
        if is_coordination_lock(&relative) {
            continue;
        }
        copy_file_exact(&path, &destination.join(relative))?;
    }
    Ok(())
}

async fn merge_databases<R: RegistryRuntime>(
    resolved: &ResolvedPlan,
    ledger: &mut ConsolidationLedger,
    registry: &R,
) -> Result<()> {
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
            Some(sqlite::plan_session_offsets(&target_sessions, &source_sessions, registry).await?);
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
    sqlite::merge_sessions(
        &target_sessions,
        &source_sessions,
        &target_input,
        &resolved.report.source.project_id,
        offsets,
        registry,
    )
    .await?;
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

#[doc(hidden)]
pub fn input_database_paths(resolved: &ResolvedPlan) -> Result<Vec<PathBuf>> {
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
        let path = confined_branch_graph_path(root, &entry.db_file)?;
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

fn confined_branch_graph_path(root: &Path, db_file: &str) -> Result<PathBuf> {
    let relative = Path::new(db_file);
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(config_error(format!(
            "branch graph path '{db_file}' is not a normalized store-relative path"
        )));
    }
    let path = root.join(relative);
    let canonical_root = root.canonicalize().map_err(io_error)?;
    let canonical_path = path.canonicalize().map_err(|error| {
        config_error(format!(
            "branch graph '{}' is missing: {error}",
            path.display()
        ))
    })?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err(config_error(format!(
            "branch graph '{}' escapes profile shard '{}'",
            path.display(),
            root.display()
        )));
    }
    if !canonical_path.is_file() {
        return Err(config_error(format!(
            "branch graph '{}' is not a file",
            path.display()
        )));
    }
    Ok(path)
}

#[doc(hidden)]
pub fn same_path(left: &Path, right: &Path) -> bool {
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
