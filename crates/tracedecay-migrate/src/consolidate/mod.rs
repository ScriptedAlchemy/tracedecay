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
mod runtime;
pub(in crate) mod sqlite;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_domain::RefId;
use tracedecay_store::{CodeShardScopeV1, ProjectId, StoreIncarnationV1, StoreShardIdV1};

use crate::root_seam::db::engine::params;
use evidence::{GraphStoreEvidence, InputReadEvidence, capture_input_evidence};
#[cfg(test)]
use files::sqlite_sidecar;
use files::{
    copy_file_atomic, copy_file_exact, copy_sqlite_family_exact, excluded_source_artifact,
    file_digest, is_coordination_lock, is_reference_artifact, is_runtime_lock, is_sqlite_database,
    is_sqlite_sidecar, relative_file_map, tree_stats,
};
use finalize::{cut_over_markers, register_destination, verify_destination};
use preflight::{acquire_store_locks, ensure_profile_offline, preflight_disk_space};
use prepare::prepare_destination;
use runtime::{
    ConsolidationArtifactAuthorityV1, ConsolidationArtifactRecordV1, ConsolidationArtifactRoleV1,
    ConsolidationRuntimeOwnerV1, FrozenInputRuntimeSetV1,
};

use crate::root_seam::branch_meta::{self, BranchEntry, BranchMeta};
use crate::root_seam::errors::{Result, TraceDecayError};
use crate::root_seam::global_db::{
    GraphScopeUpsert, RegisteredGlobalDb, StoreArtifactUpsert, StoreInstanceUpsert,
};
use crate::root_seam::storage::{
    self, EnrollmentMarker, PrivateStoreIo, StorageMode, StoreKind, StoreLayout, StoreManifest,
};

const LEDGER_SCHEMA_VERSION: u32 = 3;
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
    #[serde(default)]
    artifact_records: Vec<ConsolidationArtifactRecordV1>,
}

#[derive(Debug, Default)]
pub(crate) struct ManifestRetirementReport {
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

struct ResolvedPlan {
    report: ConsolidationReport,
    input_fingerprint: String,
    source_layout: StoreLayout,
    target_layout: StoreLayout,
    source_meta: BranchMeta,
    target_meta: BranchMeta,
    evidence: Arc<InputReadEvidence>,
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

pub async fn plan(options: &ConsolidationOptions) -> Result<ConsolidationReport> {
    ensure_profile_offline(options)?;
    let lifecycle = crate::root_seam::lifecycle_lease::acquire_exclusive_for_profile(
        &options.profile_root,
        "profile shard consolidation plan",
    )?;
    let _database_scope = crate::root_seam::db::enter_maintenance_database_scope(
        &lifecycle,
        &options.profile_root,
        "profile shard consolidation plan",
    )?;
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
    let lifecycle = crate::root_seam::lifecycle_lease::acquire_exclusive_for_profile(
        &options.profile_root,
        "profile shard consolidation",
    )?;
    let database_scope = crate::root_seam::db::enter_maintenance_database_scope(
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
    let source_graphs = graph_db_paths(&resolved.source_layout, &resolved.source_meta)?;
    let target_graphs = graph_db_paths(&resolved.target_layout, &resolved.target_meta)?;
    let session_paths = vec![
        resolved.source_layout.sessions_db_path.clone(),
        resolved.target_layout.sessions_db_path.clone(),
    ];
    resolved
        .evidence
        .validate(&source_graphs, &target_graphs, &session_paths)?;
    let frozen_records = frozen_input_records(&resolved)?;
    let profile = crate::root_seam::daemon::profile_identity::load_or_create(&options.profile_root)?;
    let profile_shard =
        StoreShardIdV1::profile(profile.brain_id().clone(), profile.profile_id().clone());
    let frozen = FrozenInputRuntimeSetV1::acquire(
        &options.profile_root,
        &lifecycle,
        &database_scope,
        profile_shard,
        &frozen_records,
    )
    .await?;
    // Advisory file locks do not cover old or direct MCP writers. Recompute
    // and copy every source while exact brokered writer reservations are held.
    let frozen_result = async {
        let locked =
            resolve_plan_inner(options, true, Some(Arc::clone(&resolved.evidence))).await?;
        if locked.report.confirmation_token != confirmation_token {
            return Err(config_error(format!(
                "input stores changed after the dry-run; rerun it and pass --confirm-token {}",
                locked.report.confirmation_token
            )));
        }
        preflight_disk_space(&locked)?;
        let ledger_path = locked.report.ledger_path.clone();
        let mut ledger = load_or_create_ledger(&locked, &ledger_path)?;
        validate_ledger(&ledger, &locked)?;
        if ledger.state == ConsolidationState::Planned {
            backup_store(&locked.source_layout, &locked.report.backup_root)?;
            backup_store(&locked.target_layout, &locked.report.backup_root)?;
            ledger.state = ConsolidationState::BackupsReady;
            save_ledger(&ledger_path, &ledger)?;
            maybe_stop(&ledger.state, stop_after.as_ref())?;
        }
        if ledger.state == ConsolidationState::BackupsReady {
            if prepare_stop.is_some() {
                prepare::prepare_destination_with_stop(&locked, prepare_stop)?;
            } else {
                prepare_destination(&locked)?;
            }
            prepare_database_artifacts(&locked)?;
            ledger.artifact_records = capture_artifact_records(&locked)?;
            ledger.state = ConsolidationState::DestinationReady;
            save_ledger(&ledger_path, &ledger)?;
            maybe_stop(&ledger.state, stop_after.as_ref())?;
        }
        Ok((locked, ledger_path, ledger))
    }
    .await;
    let cleanup = frozen.release_and_join().await;
    let (resolved, ledger_path, mut ledger) = match (frozen_result, cleanup) {
        (Ok(value), Ok(())) => value,
        (Err(error), Ok(())) => return Err(error),
        (Ok(_), Err(error)) => return Err(error),
        (Err(error), Err(cleanup_error)) => {
            return Err(config_error(format!(
                "{error}; frozen-input cleanup also failed: {cleanup_error}"
            )));
        }
    };

    if ledger.state == ConsolidationState::DestinationReady {
        validate_artifact_records(&resolved, &ledger.artifact_records)?;
        merge_databases(&resolved, &mut ledger, &lifecycle, &database_scope).await?;
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
        let (_runtime_registry, global_db) =
            mount_registered_profile_database(&options.profile_root).await?;
        register_destination(&resolved, global_db.as_ref()).await?;
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
        let (_runtime_registry, global_db) =
            mount_registered_profile_database(&options.profile_root).await?;
        finalize_applied_consolidation(&options.profile_root, &global_db, &ledger).await?;
    }

    let mut report = resolved.report;
    report.state = ledger.state;
    report.dry_run = false;
    Ok(report)
}

async fn mount_registered_profile_database(
    profile_root: &Path,
) -> Result<(
    crate::root_seam::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1,
    Arc<RegisteredGlobalDb>,
)> {
    let identity = crate::root_seam::daemon::profile_identity::load_or_create(profile_root)?;
    let runtime_registry =
        crate::root_seam::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
            identity,
        )
        .await?;
    let database = runtime_registry.profile_database().await?;
    Ok((runtime_registry, database))
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
    let git_common_dir = crate::root_seam::worktree::git_common_dir(&project_root)
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
            Arc::new(
                capture_input_evidence(
                    &source_graphs,
                    &target_graphs,
                    &session_paths,
                    scratch_root.path(),
                )
                .await?,
            )
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
    let state =
        load_ledger(&ledger_path)?.map_or(ConsolidationState::Planned, |ledger| ledger.state);
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
        && crate::root_seam::worktree::git_common_dir(&candidate.project_root)
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
    // A repository split across more than two shards is consolidated pairwise:
    // the named source and target must both claim this identity, while any
    // additional claimants stay untouched (and keep failing resolution closed)
    // until their own explicit pass.
    if !actual.is_superset(&expected) {
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
            let default_branch = crate::root_seam::branch::detect_default_branch(&layout.project_root)
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
            .map(crate::root_seam::branch::sanitize_branch_name)
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
    sessions: &crate::root_seam::sqlite_read_snapshot::SnapshotSet,
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
            "external-source reducer state: union by immutable binding identity; byte-equivalent rows deduplicate and divergent histories abort without choosing a winner".to_string(),
            "sessions: union by provider/session id; time bounds widen and non-null target fields win".to_string(),
            "session-message projections: the selected target row remains canonical; divergent source rows and their overlapping parent-linked session family are preserved as active consolidated/<source-project-id>/<message-id> variants".to_string(),
            "LCM raw messages: the selected target row remains canonical; source rows receive active variants only for content-hash divergence, while equal-content representation drift deduplicates to the target raw family".to_string(),
            "LCM external payloads and summaries: identical identities deduplicate; divergent content is a hard error".to_string(),
            "branch graphs: target branches retain their names; every source branch is preserved under consolidated/<source-id>/...".to_string(),
            "artifact paths: identical files deduplicate; divergent non-reference files are preserved under consolidation-preserved; divergent payload/handle files are a hard error".to_string(),
        ],
    })
}

pub(crate) fn destination_project_id(git_common_dir: &Path, source: &str, target: &str) -> String {
    let mut ids = [source, target];
    ids.sort_unstable();
    let mut hash = Sha256::new();
    hash.update(b"tracedecay-profile-consolidation-v1\0");
    hash.update(
        crate::root_seam::lifecycle_lease::canonical_or_original(git_common_dir)
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
                ConsolidationState::Planned | ConsolidationState::BackupsReady
            ) {
                return Err(config_error(format!(
                    "consolidation ledger v{} cannot be migrated safely after destination authority publication state {:?}",
                    ledger.schema_version, ledger.state
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
        artifact_records: Vec::new(),
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

pub(crate) async fn retire_applied_input_manifests(
    profile_root: &Path,
    global_db: &RegisteredGlobalDb,
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

    let mut applied = Vec::new();
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
        applied.push((ledger_path, ledger));
    }
    // Index every project id consumed as a source or target by an applied
    // ledger in a single pass, keyed by (canonicalized git_common_dir,
    // project_id). `applied` is ordered by ledger filename (which encodes
    // migration id, not a real timestamp), so no chronological ordering is
    // available or consulted here: this index only records whether some
    // *other* applied ledger's source or target lines up with a given
    // ledger's destination, regardless of which one applied "first".
    let mut consumers: BTreeMap<(PathBuf, String), Vec<&str>> = BTreeMap::new();
    for (_, other) in &applied {
        let git_common_dir = crate::root_seam::lifecycle_lease::canonical_or_original(&other.git_common_dir);
        consumers
            .entry((git_common_dir.clone(), other.source_project_id.clone()))
            .or_default()
            .push(other.migration_id.as_str());
        consumers
            .entry((git_common_dir, other.target_project_id.clone()))
            .or_default()
            .push(other.migration_id.as_str());
    }
    for (ledger_path, ledger) in &applied {
        // A destination consumed as the source or target of another applied
        // consolidation of the same repository was consolidated forward: its
        // markers now identify the newer destination, so this ledger can no
        // longer validate — and no longer needs to. Its inputs were retired
        // when it applied; leave the ledger as audit history. This match is
        // keyed strictly on project-id linkage, not on application order, so
        // a forward-then-back pair (A's destination feeding B and B's
        // destination feeding A) can legitimately mark both as superseded;
        // that is a property of the data, not an ordering bug.
        let git_common_dir = crate::root_seam::lifecycle_lease::canonical_or_original(&ledger.git_common_dir);
        let superseded = consumers
            .get(&(git_common_dir, ledger.destination_project_id.clone()))
            .is_some_and(|migration_ids| {
                migration_ids
                    .iter()
                    .any(|migration_id| *migration_id != ledger.migration_id)
            });
        if superseded {
            tracing::warn!(
                ledger_path = %ledger_path.display(),
                "consolidation ledger superseded by another applied consolidation; skipping retirement validation"
            );
            continue;
        }
        match finalize_applied_consolidation(profile_root, global_db, ledger).await {
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

async fn finalize_applied_consolidation(
    profile_root: &Path,
    global_db: &RegisteredGlobalDb,
    ledger: &ConsolidationLedger,
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
    // Registry retirement is the only awaited mutation in this finalization
    // step. Complete it first so cancellation cannot leave durable retired
    // manifests while the registry still advertises legacy owners. The file
    // operations below contain no suspension point, making the transition
    // cancellation-atomic from the caller's perspective.
    let retired_registry_projects =
        retire_legacy_registry_owners(global_db, profile_root, ledger).await?;
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

async fn retire_legacy_registry_owners(
    db: &RegisteredGlobalDb,
    _profile_root: &Path,
    ledger: &ConsolidationLedger,
) -> Result<usize> {
    let transaction = db
        .begin_write_transaction()
        .await
        .map_err(|error| config_error(format!("could not begin registry cleanup: {error}")))?;
    let conn = &transaction;

    #[cfg(test)]
    {
        let ledger_root = _profile_root.join(LEDGER_DIR);
        let injected_failure = ledger_root.join(".fail-registry-retirement-once");
        if injected_failure.is_file() {
            let _ = fs::remove_file(injected_failure);
            return Err(config_error(
                "synthetic registry retirement failure before manifest retirement",
            ));
        }
        let pause = ledger_root.join(".pause-registry-retirement");
        if pause.is_file() {
            fs::write(ledger_root.join(".registry-retirement-paused"), b"paused")
                .map_err(io_error)?;
            std::future::pending::<()>().await;
        }
    }

    let result = async {
        let canonical_root = RegisteredGlobalDb::canonical_project_key(&ledger.project_root);
        let root_alias = RegisteredGlobalDb::project_path_alias_key(&ledger.project_root);
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
        // Live registration may legitimately redirect a linked-worktree root
        // to the primary checkout; repository identity is the common dir.
        let registered_root_in_family = registered_root == canonical_root
            || same_path(Path::new(&registered_common), &ledger.git_common_dir);
        if !registered_root_in_family
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
                params![root_alias.as_str()],
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

        // Registry rows for the repository family are keyed by the shared git
        // common dir, not by one checkout path: worktree registrations
        // redirect canonical_root to the primary checkout, so a consolidation
        // driven from a linked worktree must retire and verify owners by
        // repository identity. Rows outside this consolidation's trio (e.g.
        // further legacy shards awaiting their own pass) are left untouched.
        let canonical_common = RegisteredGlobalDb::canonical_project_key(&ledger.git_common_dir);
        let mut rows = conn
            .query(
                "SELECT project_id FROM code_projects
                 WHERE git_common_dir=?1 AND project_id IN (?2, ?3, ?4)
                 ORDER BY project_id",
                params![
                    canonical_common.as_str(),
                    ledger.source_project_id.as_str(),
                    ledger.target_project_id.as_str(),
                    ledger.destination_project_id.as_str()
                ],
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
        if !owners.contains(&ledger.destination_project_id) {
            return Err(config_error(format!(
                "canonical root has unexpected registry owners: {owners:?}"
            )));
        }

        // Old project IDs can have been rebound to another repository. Match
        // the repository identity so those moved rows survive.
        let deleted = conn
            .execute(
                "DELETE FROM code_projects
                 WHERE project_id IN (?1, ?2)
                   AND git_common_dir=?3",
                params![
                    ledger.source_project_id.as_str(),
                    ledger.target_project_id.as_str(),
                    canonical_common.as_str()
                ],
            )
            .await
            .map_err(|error| {
                config_error(format!("could not retire legacy registry owners: {error}"))
            })?;

        let mut rows = conn
            .query(
                "SELECT project_id FROM code_projects
                 WHERE git_common_dir=?1 AND project_id IN (?2, ?3, ?4)
                 ORDER BY project_id",
                params![
                    canonical_common.as_str(),
                    ledger.source_project_id.as_str(),
                    ledger.target_project_id.as_str(),
                    ledger.destination_project_id.as_str()
                ],
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
        Ok(deleted) => {
            transaction.commit().await.map_err(|error| {
                config_error(format!("could not commit legacy registry cleanup: {error}"))
            })?;
            Ok(deleted)
        }
        Err(error) => Err(error),
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

fn prepare_database_artifacts(resolved: &ResolvedPlan) -> Result<()> {
    let destination = &resolved.report.destination_data_root;
    let input_root = destination.join(INPUT_DIR);
    fs::create_dir_all(&input_root).map_err(io_error)?;
    let source_sessions = input_root.join("source-sessions.db");
    if !source_sessions.is_file() {
        copy_sqlite_family_exact(&resolved.source_layout.sessions_db_path, &source_sessions)?;
    }
    let target_input = input_root.join("target-sessions.db");
    if !target_input.is_file() {
        copy_sqlite_family_exact(
            &destination.join(storage::SESSIONS_DB_FILENAME),
            &target_input,
        )?;
    }
    Ok(())
}

fn artifact_authorities(resolved: &ResolvedPlan) -> Result<Vec<ConsolidationArtifactAuthorityV1>> {
    let destination = &resolved.report.destination_data_root;
    let layout = layout_for_id(
        &resolved.report.project_root,
        destination
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| config_error("destination shard has no profile root"))?,
        &resolved.report.destination_project_id,
    )?;
    let meta = load_required_branch_meta(&layout)?;
    let profile_root = destination
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| config_error("destination shard has no profile root"))?;
    let profile = crate::root_seam::daemon::profile_identity::load_or_create(profile_root)?;
    let indexing = crate::root_seam::daemon::code_index_scheduler::identity::IndexingIdentityV1::resolve(
        &resolved.report.project_root,
    )
    .map_err(|error| config_error(error.to_string()))?;
    let destination_project = canonical_project_id(&resolved.report.destination_project_id)?;
    let mut branches = meta.branches.iter().collect::<Vec<_>>();
    branches.sort_by(|(left_name, left), (right_name, right)| {
        let left_default = *left_name == &meta.default_branch;
        let right_default = *right_name == &meta.default_branch;
        right_default
            .cmp(&left_default)
            .then_with(|| left.db_file.cmp(&right.db_file))
    });
    let graph_incarnation =
        StoreIncarnationV1::new(1).map_err(|error| config_error(error.to_string()))?;
    let mut authorities = Vec::with_capacity(branches.len() + 3);
    for (branch, entry) in branches {
        let (role, scope) = if branch == &meta.default_branch {
            (
                ConsolidationArtifactRoleV1::DestinationCodeGraph,
                CodeShardScopeV1::Worktree {
                    worktree_id: indexing.worktree_id().clone(),
                },
            )
        } else {
            let ref_name = if branch.starts_with("refs/heads/") {
                branch.clone()
            } else {
                format!("refs/heads/{branch}")
            };
            let ref_id = RefId::new(ref_name).map_err(|error| config_error(error.to_string()))?;
            (
                ConsolidationArtifactRoleV1::SourceCodeGraphInput,
                CodeShardScopeV1::Branch {
                    worktree_id: indexing.worktree_id().clone(),
                    ref_id,
                },
            )
        };
        let shard_id = StoreShardIdV1::code(
            profile.brain_id().clone(),
            profile.profile_id().clone(),
            destination_project.clone(),
            indexing.repository_id().clone(),
            scope,
        );
        authorities.push(ConsolidationArtifactAuthorityV1::new(
            role,
            shard_id,
            graph_incarnation,
            PathBuf::from(&entry.db_file),
        )?);
    }
    let session_incarnation =
        |value| StoreIncarnationV1::new(value).map_err(|error| config_error(error.to_string()));
    for (role, project, incarnation, relative) in [
        (
            ConsolidationArtifactRoleV1::DestinationSessions,
            canonical_project_id(&resolved.report.destination_project_id)?,
            session_incarnation(1)?,
            PathBuf::from(storage::SESSIONS_DB_FILENAME),
        ),
        (
            ConsolidationArtifactRoleV1::SourceSessionsInput,
            canonical_project_id(&resolved.report.source.project_id)?,
            session_incarnation(2)?,
            PathBuf::from(INPUT_DIR).join("source-sessions.db"),
        ),
        (
            ConsolidationArtifactRoleV1::TargetSessionsInput,
            canonical_project_id(&resolved.report.target.project_id)?,
            session_incarnation(3)?,
            PathBuf::from(INPUT_DIR).join("target-sessions.db"),
        ),
    ] {
        authorities.push(ConsolidationArtifactAuthorityV1::new(
            role,
            StoreShardIdV1::project_sessions(
                profile.brain_id().clone(),
                profile.profile_id().clone(),
                project,
            ),
            incarnation,
            relative,
        )?);
    }
    Ok(authorities)
}

fn frozen_input_records(resolved: &ResolvedPlan) -> Result<Vec<ConsolidationArtifactRecordV1>> {
    let profile_root = resolved
        .report
        .destination_data_root
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| config_error("destination shard has no profile root"))?
        .canonicalize()
        .map_err(io_error)?;
    let profile = crate::root_seam::daemon::profile_identity::load_or_create(&profile_root)?;
    let indexing = crate::root_seam::daemon::code_index_scheduler::identity::IndexingIdentityV1::resolve(
        &resolved.report.project_root,
    )
    .map_err(|error| config_error(error.to_string()))?;
    let incarnation =
        StoreIncarnationV1::new(1).map_err(|error| config_error(error.to_string()))?;
    let mut authorities = BTreeMap::new();
    let mut physical_owners = BTreeMap::new();
    for (project_id, layout, meta) in [
        (
            resolved.report.source.project_id.as_str(),
            &resolved.source_layout,
            &resolved.source_meta,
        ),
        (
            resolved.report.target.project_id.as_str(),
            &resolved.target_layout,
            &resolved.target_meta,
        ),
    ] {
        let project_id = canonical_project_id(project_id)?;
        let mut branches = meta.branches.iter().collect::<Vec<_>>();
        branches.sort_by(|(left_name, left), (right_name, right)| {
            let left_default = *left_name == &meta.default_branch;
            let right_default = *right_name == &meta.default_branch;
            right_default
                .cmp(&left_default)
                .then_with(|| left.db_file.cmp(&right.db_file))
        });
        for (branch, entry) in branches {
            let path = confined_branch_graph_path(&layout.data_root, &entry.db_file)?;
            let relative = canonical_relative_locator(&profile_root, &path)?;
            if physical_owners
                .insert(relative.clone(), project_id.clone())
                .is_some_and(|owner| owner != project_id)
            {
                return Err(config_error(
                    "consolidation projects alias the same physical graph database",
                ));
            }
            let scope = if branch == &meta.default_branch {
                CodeShardScopeV1::Worktree {
                    worktree_id: indexing.worktree_id().clone(),
                }
            } else {
                let ref_name = if branch.starts_with("refs/heads/") {
                    branch.clone()
                } else {
                    format!("refs/heads/{branch}")
                };
                CodeShardScopeV1::Branch {
                    worktree_id: indexing.worktree_id().clone(),
                    ref_id: RefId::new(ref_name)
                        .map_err(|error| config_error(error.to_string()))?,
                }
            };
            let authority = ConsolidationArtifactAuthorityV1::new(
                ConsolidationArtifactRoleV1::FrozenInputCodeGraph,
                StoreShardIdV1::code(
                    profile.brain_id().clone(),
                    profile.profile_id().clone(),
                    project_id.clone(),
                    indexing.repository_id().clone(),
                    scope,
                ),
                incarnation,
                relative.clone(),
            )?;
            authorities.entry(relative).or_insert(authority);
        }
        let relative = canonical_relative_locator(&profile_root, &layout.sessions_db_path)?;
        if physical_owners
            .insert(relative.clone(), project_id.clone())
            .is_some()
        {
            return Err(config_error(
                "session input aliases another consolidation database",
            ));
        }
        let authority = ConsolidationArtifactAuthorityV1::new(
            ConsolidationArtifactRoleV1::FrozenInputSessions,
            StoreShardIdV1::project_sessions(
                profile.brain_id().clone(),
                profile.profile_id().clone(),
                project_id,
            ),
            incarnation,
            relative.clone(),
        )?;
        authorities.insert(relative, authority);
    }
    authorities
        .into_values()
        .map(|authority| ConsolidationArtifactRecordV1::capture(&profile_root, authority))
        .collect()
}

fn canonical_relative_locator(root: &Path, path: &Path) -> Result<PathBuf> {
    let path = path.canonicalize().map_err(io_error)?;
    path.strip_prefix(root)
        .map(Path::to_path_buf)
        .map_err(|_| config_error("consolidation input database escapes its profile root"))
}

fn capture_artifact_records(resolved: &ResolvedPlan) -> Result<Vec<ConsolidationArtifactRecordV1>> {
    artifact_authorities(resolved)?
        .into_iter()
        .map(|authority| {
            ConsolidationArtifactRecordV1::capture(
                &resolved.report.destination_data_root,
                authority,
            )
        })
        .collect()
}

fn validate_artifact_records(
    resolved: &ResolvedPlan,
    records: &[ConsolidationArtifactRecordV1],
) -> Result<()> {
    for record in records {
        record.authority.validate()?;
    }
    let expected = artifact_authorities(resolved)?;
    if records.len() != expected.len() {
        return Err(config_error(
            "consolidation ledger artifact inventory is incomplete",
        ));
    }
    for expected in expected {
        let mut matching = records.iter().filter(|record| record.authority == expected);
        let Some(record) = matching.next() else {
            return Err(config_error(
                "consolidation artifact ledger authority does not match the requested artifact",
            ));
        };
        if matching.next().is_some() {
            return Err(config_error(
                "consolidation ledger contains duplicate artifact authority",
            ));
        }
        let current = ConsolidationArtifactRecordV1::capture(
            &resolved.report.destination_data_root,
            expected,
        )?;
        if current.file_identity != record.file_identity {
            return Err(config_error(
                "consolidation artifact file identity changed since DestinationReady",
            ));
        }
    }
    Ok(())
}

fn canonical_project_id(value: &str) -> Result<ProjectId> {
    ProjectId::try_from(value.to_owned()).map_err(|error| config_error(error.to_string()))
}

async fn merge_databases(
    resolved: &ResolvedPlan,
    ledger: &mut ConsolidationLedger,
    lifecycle: &crate::root_seam::lifecycle_lease::LifecycleLease,
    maintenance: &crate::root_seam::db::MaintenanceDatabaseScope<'_>,
) -> Result<()> {
    let destination = &resolved.report.destination_data_root;
    let profile_root = destination
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| config_error("destination shard has no profile root"))?;
    let profile = crate::root_seam::daemon::profile_identity::load_or_create(profile_root)?;
    let profile_shard =
        StoreShardIdV1::profile(profile.brain_id().clone(), profile.profile_id().clone());
    let authorities = artifact_authorities(resolved)?;
    let owner = ConsolidationRuntimeOwnerV1::new(
        profile_root,
        destination,
        lifecycle,
        maintenance,
        profile_shard,
        &ledger.artifact_records,
    )
    .await?;

    let operation = async {
        let graph_authorities = authorities
            .iter()
            .filter(|authority| {
                matches!(
                    authority.role,
                    ConsolidationArtifactRoleV1::DestinationCodeGraph
                        | ConsolidationArtifactRoleV1::SourceCodeGraphInput
                )
            })
            .collect::<Vec<_>>();
        let (target_graph, source_graphs) = graph_authorities
            .split_first()
            .ok_or_else(|| config_error("consolidation has no destination graph authority"))?;
        let target_graph_record = artifact_record(&ledger.artifact_records, target_graph)?;
        let mut maxima = if ledger.graph_offsets.is_empty() {
            let target_mount = owner.mount(target_graph, target_graph_record).await?;
            let maxima = sqlite::registered_graph_maxima(target_mount.database()?).await;
            let (maxima, token) = target_mount.finish_operation(maxima).await?;
            drop(token);
            Some(maxima)
        } else {
            None
        };

        let mut graph_tokens = Vec::with_capacity(source_graphs.len());
        let mut computed_offsets = Vec::with_capacity(source_graphs.len());
        for source in source_graphs {
            let record = artifact_record(&ledger.artifact_records, source)?;
            let mounted = owner.mount(source, record).await?;
            if let Some(maxima) = maxima.as_mut() {
                let source_maxima = sqlite::registered_graph_maxima(mounted.database()?).await;
                let (source_maxima, token) = mounted.finish_operation(source_maxima).await?;
                computed_offsets.push(sqlite::graph_offsets((*source).clone(), *maxima));
                sqlite::advance_graph_maxima(maxima, source_maxima)?;
                graph_tokens.push(token);
            } else {
                let ((), token) = mounted.finish_operation(Ok(())).await?;
                graph_tokens.push(token);
            }
        }
        if ledger.graph_offsets.is_empty() {
            ledger.graph_offsets = computed_offsets;
            save_ledger(&resolved.report.ledger_path, ledger)?;
        } else if ledger.graph_offsets.len() != graph_tokens.len()
            || ledger
                .graph_offsets
                .iter()
                .zip(source_graphs)
                .any(|(offset, source)| &offset.source_authority != *source)
        {
            return Err(config_error(
                "consolidation graph offsets do not match exact source authorities",
            ));
        }
        let target_mount = owner.mount(target_graph, target_graph_record).await?;
        let graph_sources = ledger
            .graph_offsets
            .iter()
            .zip(graph_tokens)
            .collect::<Vec<_>>();
        let merged =
            sqlite::merge_registered_graph_facts(target_mount.database()?, graph_sources).await;
        let ((), token) = target_mount.finish_operation(merged).await?;
        drop(token);

        let destination_sessions = authority_for_role(
            &authorities,
            ConsolidationArtifactRoleV1::DestinationSessions,
        )?;
        let source_sessions = authority_for_role(
            &authorities,
            ConsolidationArtifactRoleV1::SourceSessionsInput,
        )?;
        let target_input = authority_for_role(
            &authorities,
            ConsolidationArtifactRoleV1::TargetSessionsInput,
        )?;
        let destination_sessions_record =
            artifact_record(&ledger.artifact_records, destination_sessions)?;
        let mounted = owner
            .mount(destination_sessions, destination_sessions_record)
            .await?;
        let offsets = async {
            sqlite::normalize_registered_sessions(mounted.database()?).await?;
            if ledger.session_offsets.is_none() {
                sqlite::registered_session_offsets(mounted.database()?)
                    .await
                    .map(Some)
            } else {
                Ok(None)
            }
        }
        .await;
        let (computed_session_offsets, token) = mounted.finish_operation(offsets).await?;
        drop(token);
        if let Some(offsets) = computed_session_offsets {
            ledger.session_offsets = Some(offsets);
            save_ledger(&resolved.report.ledger_path, ledger)?;
        }

        let source_record = artifact_record(&ledger.artifact_records, source_sessions)?;
        let mounted = owner.mount(source_sessions, source_record).await?;
        let prepared = async {
            sqlite::normalize_registered_sessions(mounted.database()?).await?;
            sqlite::validate_registered_session_source(mounted.database()?).await
        }
        .await;
        let ((), source_token) = mounted.finish_operation(prepared).await?;

        let target_input_record = artifact_record(&ledger.artifact_records, target_input)?;
        let mounted = owner.mount(target_input, target_input_record).await?;
        let ((), target_input_token) = mounted.finish_operation(Ok(())).await?;

        let offsets = ledger
            .session_offsets
            .clone()
            .ok_or_else(|| config_error("session merge offsets are missing from the ledger"))?;
        let mounted = owner
            .mount(destination_sessions, destination_sessions_record)
            .await?;
        let merged = async {
            sqlite::normalize_registered_sessions(mounted.database()?).await?;
            sqlite::merge_registered_sessions(
                mounted.database()?,
                source_token,
                target_input_token,
                &resolved.report.source.project_id,
                &offsets,
            )
            .await
        }
        .await;
        let ((), token) = mounted.finish_operation(merged).await?;
        drop(token);
        Ok(())
    }
    .await;
    let cleanup = owner.close().await;
    match (operation, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup_error)) => Err(config_error(format!(
            "{error}; destination-runtime cleanup also failed: {cleanup_error}"
        ))),
    }
}

fn artifact_record<'a>(
    records: &'a [ConsolidationArtifactRecordV1],
    expected: &ConsolidationArtifactAuthorityV1,
) -> Result<&'a ConsolidationArtifactRecordV1> {
    records
        .iter()
        .find(|record| record.authority == *expected)
        .ok_or_else(|| config_error("exact consolidation artifact record is missing"))
}

fn authority_for_role(
    authorities: &[ConsolidationArtifactAuthorityV1],
    role: ConsolidationArtifactRoleV1,
) -> Result<&ConsolidationArtifactAuthorityV1> {
    let mut matches = authorities
        .iter()
        .filter(|authority| authority.role == role);
    let authority = matches
        .next()
        .ok_or_else(|| config_error("required consolidation artifact role is missing"))?;
    if matches.next().is_some() {
        return Err(config_error(
            "consolidation artifact role has multiple authorities",
        ));
    }
    Ok(authority)
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

#[cfg(test)]
fn input_database_paths(resolved: &ResolvedPlan) -> Result<Vec<PathBuf>> {
    database_paths_for_layouts(
        &resolved.source_layout,
        &resolved.source_meta,
        &resolved.target_layout,
        &resolved.target_meta,
    )
}

#[cfg(test)]
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
    let main = root.join(crate::root_seam::config::DB_FILENAME);
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

fn same_path(left: &Path, right: &Path) -> bool {
    crate::root_seam::lifecycle_lease::canonical_or_original(left)
        == crate::root_seam::lifecycle_lease::canonical_or_original(right)
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
        #[cfg(unix)]
        let mut builder = fs::DirBuilder::new();
        #[cfg(not(unix))]
        let builder = fs::DirBuilder::new();
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
