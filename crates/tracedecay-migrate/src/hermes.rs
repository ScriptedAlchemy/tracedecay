//! One-time migration of historical Hermes-local `TraceDecay` session stores.
//!
//! Runtime storage never resolves through Hermes. This module only scans the
//! historical, bounded locations older installers could use and copies a
//! provably project-owned store into that project's user-profile shard.
//! Sources are opened read-only and are never deleted.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use libsql::{Connection, Value, params};
use sha2::{Digest, Sha256};

use crate::db::Database;
use crate::memory::store::MemoryStore;
use crate::registry_adapter::{RegistryDatabase, RegistryRuntime, canonical_project_key};

mod session_merge;

use session_merge::merge_snapshot;

pub struct LegacyHermesStateImport {
    pub sessions_upserted: u64,
    pub messages_upserted: u64,
}

pub trait HermesStateImporter {
    fn user_sessions_db_path(&self, profile_root: &Path) -> PathBuf;

    async fn ingest_legacy_pinned_profile(
        &self,
        target_sessions_db_path: &Path,
        profile_dir: &Path,
        project_root: &Path,
    ) -> Result<LegacyHermesStateImport, String>;
}

const LEDGER_DIR: &str = "migration-ledger/hermes-legacy";
const COPIED_TABLES: &[&str] = &[
    "sessions",
    "session_messages",
    "lcm_external_payloads",
    "lcm_raw_messages",
    "lcm_summary_nodes",
    "lcm_summary_sources",
    "lcm_lifecycle_state",
    "lcm_maintenance_debt",
];
const COPIED_MEMORY_TABLES: &[&str] = &[
    "memory_facts",
    "memory_entities",
    "memory_fact_entities",
    "memory_feedback_events",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyHermesMigration {
    pub source_db: PathBuf,
    pub target_project: PathBuf,
    pub rows_copied: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyHermesMigrationIssue {
    pub source_db: PathBuf,
    pub reason: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LegacyHermesMigrationReport {
    pub migrated: Vec<LegacyHermesMigration>,
    pub already_migrated: Vec<LegacyHermesMigration>,
    pub unresolved: Vec<LegacyHermesMigrationIssue>,
    pub failed: Vec<LegacyHermesMigrationIssue>,
}

/// Migrates historical stores below the standard user Hermes integration into
/// the normal `TraceDecay` user profile. No environment or working-directory
/// override can redirect discovery.
pub async fn migrate_legacy_hermes_stores_with_runtime<R, F, H>(
    user_home: &Path,
    registry: &R,
    read_pinned_project_root: &F,
    state_importer: &H,
) -> LegacyHermesMigrationReport
where
    R: RegistryRuntime,
    F: Fn(&Path) -> Option<String>,
    H: HermesStateImporter,
{
    let Ok(profile_root) = crate::storage::default_profile_root() else {
        return LegacyHermesMigrationReport {
            failed: vec![LegacyHermesMigrationIssue {
                source_db: user_home.join(".hermes/.tracedecay/sessions.db"),
                reason: "could not resolve the TraceDecay user-profile store".to_string(),
            }],
            ..LegacyHermesMigrationReport::default()
        };
    };
    let hermes_homes = [user_home.join(".hermes")];
    migrate_legacy_hermes_stores_inner(
        user_home,
        &profile_root,
        &hermes_homes,
        None,
        registry,
        read_pinned_project_root,
        state_importer,
    )
    .await
}

/// Explicit `TraceDecay` profile-root seam used by migration tests. The source
/// root remains the user's standard home; the second argument controls only
/// the destination `TraceDecay` profile.
pub async fn migrate_legacy_hermes_stores_to_with_runtime<R, F, H>(
    user_home: &Path,
    tracedecay_profile_root: &Path,
    registry: &R,
    read_pinned_project_root: &F,
    state_importer: &H,
) -> LegacyHermesMigrationReport
where
    R: RegistryRuntime,
    F: Fn(&Path) -> Option<String>,
    H: HermesStateImporter,
{
    migrate_legacy_hermes_stores_inner(
        user_home,
        tracedecay_profile_root,
        &[user_home.join(".hermes")],
        None,
        registry,
        read_pinned_project_root,
        state_importer,
    )
    .await
}

async fn migrate_legacy_hermes_stores_inner<R, F, H>(
    user_home: &Path,
    tracedecay_profile_root: &Path,
    hermes_homes: &[PathBuf],
    fail_after_table: Option<&str>,
    registry: &R,
    read_pinned_project_root: &F,
    state_importer: &H,
) -> LegacyHermesMigrationReport
where
    R: RegistryRuntime,
    F: Fn(&Path) -> Option<String>,
    H: HermesStateImporter,
{
    let lifecycle = match crate::lifecycle_lease::acquire_exclusive_for_profile(
        tracedecay_profile_root,
        "legacy Hermes store migration",
    ) {
        Ok(lifecycle) => lifecycle,
        Err(error) => {
            return migration_authority_failure(tracedecay_profile_root, error.to_string());
        }
    };
    let _database_scope = match crate::db::enter_maintenance_database_scope(
        &lifecycle,
        tracedecay_profile_root,
        "legacy Hermes store migration",
    ) {
        Ok(scope) => scope,
        Err(error) => {
            return migration_authority_failure(tracedecay_profile_root, error.to_string());
        }
    };
    let profile_dirs = legacy_profile_dirs_for_homes(hermes_homes);
    let mut report = LegacyHermesMigrationReport::default();
    for candidate in legacy_store_candidates(&profile_dirs, tracedecay_profile_root) {
        let source_db = candidate.primary_path().to_path_buf();
        match migrate_candidate(
            user_home,
            hermes_homes,
            &candidate,
            tracedecay_profile_root,
            fail_after_table,
            registry,
            read_pinned_project_root,
            state_importer,
        )
        .await
        {
            Ok(CandidateOutcome::Migrated(migration, preserved_memory)) => {
                if let Err(reason) = remove_legacy_registry_metadata(
                    tracedecay_profile_root,
                    candidate.legacy_registry_project_id.as_deref(),
                    &candidate.profile_dir,
                    registry,
                )
                .await
                {
                    report.failed.push(LegacyHermesMigrationIssue {
                        source_db: source_db.clone(),
                        reason,
                    });
                }
                report.migrated.push(migration);
                report.unresolved.extend(preserved_memory);
            }
            Ok(CandidateOutcome::AlreadyMigrated(migration, preserved_memory)) => {
                if let Err(reason) = remove_legacy_registry_metadata(
                    tracedecay_profile_root,
                    candidate.legacy_registry_project_id.as_deref(),
                    &candidate.profile_dir,
                    registry,
                )
                .await
                {
                    report.failed.push(LegacyHermesMigrationIssue {
                        source_db: source_db.clone(),
                        reason,
                    });
                }
                report.already_migrated.push(migration);
                report.unresolved.extend(preserved_memory);
            }
            Err(CandidateError::Unresolved(reason)) => {
                report
                    .unresolved
                    .push(LegacyHermesMigrationIssue { source_db, reason });
            }
            Err(CandidateError::Failed(reason)) => {
                report
                    .failed
                    .push(LegacyHermesMigrationIssue { source_db, reason });
            }
        }
    }
    for profile_dir in profile_dirs {
        let state_db = profile_dir.join("state.db");
        if !state_db.is_file()
            || read_pinned_project_root(&profile_dir.join("config.yaml")).is_none()
        {
            continue;
        }
        match migrate_legacy_state_store(
            user_home,
            hermes_homes,
            &profile_dir,
            tracedecay_profile_root,
            registry,
            read_pinned_project_root,
            state_importer,
        )
        .await
        {
            Ok(CandidateOutcome::Migrated(migration, preserved_memory)) => {
                report.migrated.push(migration);
                report.unresolved.extend(preserved_memory);
            }
            Ok(CandidateOutcome::AlreadyMigrated(migration, preserved_memory)) => {
                report.already_migrated.push(migration);
                report.unresolved.extend(preserved_memory);
            }
            Err(CandidateError::Unresolved(reason)) => {
                report.unresolved.push(LegacyHermesMigrationIssue {
                    source_db: state_db,
                    reason,
                });
            }
            Err(CandidateError::Failed(reason)) => report.failed.push(LegacyHermesMigrationIssue {
                source_db: state_db,
                reason,
            }),
        }
    }
    report
}

fn migration_authority_failure(
    tracedecay_profile_root: &Path,
    reason: String,
) -> LegacyHermesMigrationReport {
    LegacyHermesMigrationReport {
        failed: vec![LegacyHermesMigrationIssue {
            source_db: tracedecay_profile_root.to_path_buf(),
            reason,
        }],
        ..LegacyHermesMigrationReport::default()
    }
}

async fn remove_legacy_registry_metadata<R: RegistryRuntime>(
    tracedecay_profile_root: &Path,
    project_id: Option<&str>,
    expected_legacy_root: &Path,
    registry_runtime: &R,
) -> Result<(), String> {
    let Some(project_id) = project_id else {
        return Ok(());
    };
    let registry_path = tracedecay_profile_root.join("global.db");
    let registry = registry_runtime
        .open_at(&registry_path)
        .await
        .ok_or_else(|| {
            format!(
                "could not open project registry '{}'",
                registry_path.display()
            )
        })?;
    let Some(project) = registry.get_code_project(project_id).await else {
        return Ok(());
    };
    if !same_path(Path::new(&project.canonical_root), expected_legacy_root)
        && !same_path(Path::new(&project.display_root), expected_legacy_root)
    {
        return Ok(());
    }
    registry
        .delete_code_projects(&[project_id.to_string()])
        .await;
    if registry.get_code_project(project_id).await.is_some() {
        return Err(format!(
            "migrated legacy sessions, but could not remove legacy Hermes registry metadata for '{project_id}'; source stores were preserved"
        ));
    }
    Ok(())
}

struct LegacyStoreCandidate {
    profile_dir: PathBuf,
    source_db: PathBuf,
    source_sessions_db: Option<PathBuf>,
    source_memory_db: Option<PathBuf>,
    legacy_registry_project_id: Option<String>,
}

impl LegacyStoreCandidate {
    fn primary_path(&self) -> &Path {
        &self.source_db
    }
}

fn legacy_store_candidates(
    profiles: &[PathBuf],
    tracedecay_profile_root: &Path,
) -> Vec<LegacyStoreCandidate> {
    let mut candidates = profiles
        .iter()
        .filter_map(|profile_dir| {
            let data_root = profile_dir.join(".tracedecay");
            let sessions_db = data_root.join(crate::storage::SESSIONS_DB_FILENAME);
            let memory_db = data_root.join(crate::config::db_filename(&data_root));
            (sessions_db.is_file() || memory_db.is_file()).then(|| LegacyStoreCandidate {
                profile_dir: profile_dir.clone(),
                source_db: if sessions_db.is_file() {
                    sessions_db.clone()
                } else {
                    memory_db.clone()
                },
                source_sessions_db: sessions_db.is_file().then_some(sessions_db),
                source_memory_db: memory_db.is_file().then_some(memory_db),
                legacy_registry_project_id: None,
            })
        })
        .collect::<Vec<_>>();

    // A short-lived historical release could create a user-profile shard
    // whose manifest identified a Hermes profile as the code project. Scan
    // only immediate project shards and accept only exact standard-profile
    // identities; unrelated profile stores are never opened.
    if let Ok(entries) = fs::read_dir(tracedecay_profile_root.join("projects")) {
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let shard = entry.path();
            let manifest_path = shard.join(crate::storage::STORE_MANIFEST_FILENAME);
            let Ok(manifest) = crate::storage::read_store_manifest(&manifest_path) else {
                continue;
            };
            let Some(profile_dir) = profiles
                .iter()
                .find(|profile| same_path(profile, &manifest.project_root))
            else {
                continue;
            };
            let sessions_db = shard.join(crate::storage::SESSIONS_DB_FILENAME);
            let memory_db = shard.join(crate::config::db_filename(&shard));
            if sessions_db.is_file() || memory_db.is_file() {
                candidates.push(LegacyStoreCandidate {
                    profile_dir: profile_dir.clone(),
                    source_db: if sessions_db.is_file() {
                        sessions_db.clone()
                    } else {
                        memory_db.clone()
                    },
                    source_sessions_db: sessions_db.is_file().then_some(sessions_db),
                    source_memory_db: memory_db.is_file().then_some(memory_db),
                    legacy_registry_project_id: manifest.project_id,
                });
            }
        }
    }
    candidates.sort_by(|left, right| left.primary_path().cmp(right.primary_path()));
    candidates.dedup_by(|left, right| {
        same_optional_path(
            left.source_sessions_db.as_deref(),
            right.source_sessions_db.as_deref(),
        ) && same_optional_path(
            left.source_memory_db.as_deref(),
            right.source_memory_db.as_deref(),
        )
    });
    candidates
}

fn same_optional_path(left: Option<&Path>, right: Option<&Path>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => same_path(left, right),
        (None, None) => true,
        _ => false,
    }
}

fn legacy_profile_dirs(hermes_home: &Path) -> Vec<PathBuf> {
    let mut profiles = vec![hermes_home.to_path_buf()];
    if !hermes_home.is_dir() {
        return profiles;
    }
    if let Ok(entries) = fs::read_dir(hermes_home.join("profiles")) {
        let mut named = entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                // Do not let a profile symlink turn this bounded scan into an
                // arbitrary filesystem walk.
                entry.file_type().ok()?.is_dir().then(|| entry.path())
            })
            .collect::<Vec<_>>();
        named.sort();
        profiles.extend(named);
    }
    profiles
}

fn legacy_profile_dirs_for_homes(hermes_homes: &[PathBuf]) -> Vec<PathBuf> {
    let mut profiles = hermes_homes
        .iter()
        .flat_map(|home| legacy_profile_dirs(home))
        .collect::<Vec<_>>();
    profiles.sort();
    profiles.dedup_by(|left, right| same_path(left, right));
    profiles
}

enum CandidateOutcome {
    Migrated(LegacyHermesMigration, Option<LegacyHermesMigrationIssue>),
    AlreadyMigrated(LegacyHermesMigration, Option<LegacyHermesMigrationIssue>),
}

enum CandidateError {
    Unresolved(String),
    Failed(String),
}

struct ResolvedTargetProject {
    root: PathBuf,
    registry_project_id: Option<String>,
    user_scope: bool,
}

struct ResolvedTargetLayout {
    sessions_db_path: PathBuf,
    graph_db_path: Option<PathBuf>,
    project_id: String,
}

async fn migrate_candidate<R, F, H>(
    user_home: &Path,
    hermes_homes: &[PathBuf],
    candidate: &LegacyStoreCandidate,
    tracedecay_profile_root: &Path,
    fail_after_table: Option<&str>,
    registry: &R,
    read_pinned_project_root: &F,
    state_importer: &H,
) -> Result<CandidateOutcome, CandidateError>
where
    R: RegistryRuntime,
    F: Fn(&Path) -> Option<String>,
    H: HermesStateImporter,
{
    let source_db = match candidate.source_sessions_db.as_deref() {
        Some(path) => Some(registry.open_read_only_at(path).await.ok_or_else(|| {
            CandidateError::Failed("could not open source read-only".to_string())
        })?),
        None => None,
    };
    if let Some(source) = source_db.as_ref() {
        source.conn().execute("BEGIN", ()).await.map_err(|error| {
            CandidateError::Failed(format!("could not snapshot source: {error}"))
        })?;
    }

    let result = migrate_candidate_snapshot(
        user_home,
        hermes_homes,
        candidate,
        source_db.as_ref().map(|db| db.conn()),
        tracedecay_profile_root,
        fail_after_table,
        registry,
        read_pinned_project_root,
        state_importer,
    )
    .await;
    let finish = match source_db.as_ref() {
        Some(source) => source.conn().execute("COMMIT", ()).await.map(|_| ()),
        None => Ok(()),
    };
    match (result, finish) {
        (Ok(outcome), Ok(())) => Ok(outcome),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(CandidateError::Failed(format!(
            "could not close source snapshot: {error}"
        ))),
    }
}

async fn migrate_legacy_state_store<R, F, H>(
    user_home: &Path,
    hermes_homes: &[PathBuf],
    profile_dir: &Path,
    tracedecay_profile_root: &Path,
    registry: &R,
    read_pinned_project_root: &F,
    state_importer: &H,
) -> Result<CandidateOutcome, CandidateError>
where
    R: RegistryRuntime,
    F: Fn(&Path) -> Option<String>,
    H: HermesStateImporter,
{
    let state_db = profile_dir.join("state.db");
    let target_project = resolve_target_project(
        None,
        &profile_dir.join("config.yaml"),
        user_home,
        hermes_homes,
        tracedecay_profile_root,
        registry,
        read_pinned_project_root,
    )
    .await
    .map_err(CandidateError::Unresolved)?;
    let target_layout =
        resolve_target_layout(&target_project, tracedecay_profile_root, state_importer)
            .await
            .map_err(|error| {
                CandidateError::Failed(format!("could not resolve target profile shard: {error}"))
            })?;
    let stats = state_importer
        .ingest_legacy_pinned_profile(
            &target_layout.sessions_db_path,
            profile_dir,
            &target_project.root,
        )
        .await
        .map_err(CandidateError::Failed)?;
    let rows_copied = stats
        .sessions_upserted
        .saturating_add(stats.messages_upserted);
    let migration = LegacyHermesMigration {
        source_db: state_db,
        target_project: target_project.root,
        rows_copied,
    };
    Ok(if rows_copied == 0 {
        CandidateOutcome::AlreadyMigrated(migration, None)
    } else {
        CandidateOutcome::Migrated(migration, None)
    })
}

async fn migrate_candidate_snapshot<R, F, H>(
    user_home: &Path,
    hermes_homes: &[PathBuf],
    candidate: &LegacyStoreCandidate,
    source: Option<&Connection>,
    tracedecay_profile_root: &Path,
    fail_after_table: Option<&str>,
    registry: &R,
    read_pinned_project_root: &F,
    state_importer: &H,
) -> Result<CandidateOutcome, CandidateError>
where
    R: RegistryRuntime,
    F: Fn(&Path) -> Option<String>,
    H: HermesStateImporter,
{
    if let Some(source) = source {
        verify_source(source)
            .await
            .map_err(CandidateError::Failed)?;
    }
    let source_schema_version = match source {
        Some(source) => source_lcm_schema_version(source)
            .await
            .map_err(CandidateError::Failed)?,
        None => 0,
    };
    if source_schema_version > tracedecay_sessions::lcm::LCM_SCHEMA_VERSION {
        return Err(CandidateError::Failed(format!(
            "source LCM schema {source_schema_version} is newer than supported schema {}",
            tracedecay_sessions::lcm::LCM_SCHEMA_VERSION
        )));
    }

    let target_project = resolve_target_project(
        source,
        &candidate.profile_dir.join("config.yaml"),
        user_home,
        hermes_homes,
        tracedecay_profile_root,
        registry,
        read_pinned_project_root,
    )
    .await
    .map_err(CandidateError::Unresolved)?;
    let preserved_memory = if target_project.user_scope {
        candidate
            .source_memory_db
            .as_ref()
            .map(|source_db| LegacyHermesMigrationIssue {
                source_db: source_db.clone(),
                reason: "unscoped legacy memory was preserved because no durable project attribution exists"
                    .to_string(),
            })
    } else {
        None
    };
    let target_layout =
        resolve_target_layout(&target_project, tracedecay_profile_root, state_importer)
            .await
            .map_err(|error| {
                CandidateError::Failed(format!("could not resolve target profile shard: {error}"))
            })?;
    if candidate
        .source_sessions_db
        .as_deref()
        .is_some_and(|source_path| same_path(source_path, &target_layout.sessions_db_path))
    {
        return Err(CandidateError::Failed(
            "source and target session databases resolve to the same path".to_string(),
        ));
    }
    if candidate
        .source_memory_db
        .as_deref()
        .zip(target_layout.graph_db_path.as_deref())
        .is_some_and(|(source_path, target_path)| same_path(source_path, target_path))
    {
        return Err(CandidateError::Failed(
            "source and target memory databases resolve to the same path".to_string(),
        ));
    }
    // Projectless sessions are safe to retain in the profile user-session
    // store. Legacy memory facts are not: without a project pin or durable
    // session attribution their scope cannot be proven, so leave that source
    // database untouched for a later explicit recovery.
    let source_memory = match candidate
        .source_memory_db
        .as_deref()
        .filter(|_| !target_project.user_scope)
    {
        Some(path) => {
            let authority = crate::db::DatabaseAuthority::for_runtime(
                path,
                "read legacy memory migration source",
            )
            .map_err(|error| CandidateError::Failed(error.to_string()))?;
            let (db, _) = Database::open_read_only(path, &authority)
                .await
                .map_err(|error| {
                    CandidateError::Failed(format!(
                        "could not open legacy memory store '{}' read-only: {error}",
                        path.display()
                    ))
                })?;
            db.conn().execute("BEGIN", ()).await.map_err(|error| {
                CandidateError::Failed(format!("could not snapshot legacy memory store: {error}"))
            })?;
            Some(db)
        }
        None => None,
    };
    let fingerprint = logical_source_fingerprint(
        source,
        candidate.primary_path(),
        source_memory
            .as_ref()
            .zip(candidate.source_memory_db.as_deref())
            .map(|(db, path)| (db.conn(), path)),
    )
    .await
    .map_err(CandidateError::Failed)?;
    let target_db = registry
        .open_at(&target_layout.sessions_db_path)
        .await
        .ok_or_else(|| CandidateError::Failed("could not open target session store".to_string()))?;
    if let Some(source) = source {
        ensure_message_identity_matches(source, target_db.conn(), "session_messages", "text")
            .await
            .map_err(CandidateError::Failed)?;
        ensure_message_identity_matches(
            source,
            target_db.conn(),
            "lcm_raw_messages",
            "content_hash",
        )
        .await
        .map_err(CandidateError::Failed)?;
    }
    let memory_rows = match source_memory.as_ref() {
        Some(source_memory) => merge_memory_snapshot(
            source_memory.conn(),
            target_layout.graph_db_path.as_deref().ok_or_else(|| {
                CandidateError::Failed("project memory target disappeared".to_string())
            })?,
        )
        .await
        .map_err(CandidateError::Failed)?,
        None => 0,
    };

    let result = merge_snapshot(
        source,
        candidate.primary_path(),
        target_db.conn(),
        &target_layout.sessions_db_path,
        &target_project.root,
        &target_layout.project_id,
        &fingerprint,
        source_schema_version,
        memory_rows,
        fail_after_table,
    )
    .await
    .map_err(CandidateError::Failed)?;
    if let Some(source_memory) = source_memory.as_ref() {
        source_memory
            .conn()
            .execute("COMMIT", ())
            .await
            .map_err(|error| {
                CandidateError::Failed(format!("could not close legacy memory snapshot: {error}"))
            })?;
    }
    let migration = LegacyHermesMigration {
        source_db: candidate.primary_path().to_path_buf(),
        target_project: target_project.root,
        rows_copied: result.rows_copied,
    };
    Ok(if result.already_migrated {
        CandidateOutcome::AlreadyMigrated(migration, preserved_memory)
    } else {
        CandidateOutcome::Migrated(migration, preserved_memory)
    })
}

async fn resolve_target_layout<H: HermesStateImporter>(
    target_project: &ResolvedTargetProject,
    tracedecay_profile_root: &Path,
    state_importer: &H,
) -> crate::errors::Result<ResolvedTargetLayout> {
    if target_project.user_scope {
        return Ok(ResolvedTargetLayout {
            sessions_db_path: state_importer.user_sessions_db_path(tracedecay_profile_root),
            graph_db_path: None,
            project_id: "user".to_string(),
        });
    }
    if let Some(project_id) = target_project.registry_project_id.as_deref() {
        if let Some(layout) =
            crate::storage::resolve_persisted_layout(&target_project.root, tracedecay_profile_root)?
        {
            if layout.identity.project_id.as_deref() != Some(project_id) {
                return Err(crate::errors::TraceDecayError::Config {
                    message: format!(
                        "registered project identity collision for '{}': registry has '{project_id}', repository has '{}'",
                        target_project.root.display(),
                        layout.identity.project_id.as_deref().unwrap_or("none")
                    ),
                });
            }
            return project_layout(layout);
        }
        return project_layout(crate::storage::profile_sharded_layout(
            &target_project.root,
            tracedecay_profile_root,
            &crate::storage::EnrollmentMarker {
                project_id: project_id.to_string(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )?);
    }

    let layout = crate::storage::resolve_layout(&target_project.root, tracedecay_profile_root)?;
    project_layout(layout)
}

fn project_layout(
    layout: crate::storage::StoreLayout,
) -> crate::errors::Result<ResolvedTargetLayout> {
    let project_id = layout.identity.project_id.clone().ok_or_else(|| {
        crate::errors::TraceDecayError::Config {
            message: "target project shard has no durable project id".to_string(),
        }
    })?;
    Ok(ResolvedTargetLayout {
        sessions_db_path: layout.sessions_db_path,
        graph_db_path: Some(layout.graph_db_path),
        project_id,
    })
}

async fn verify_source(source: &Connection) -> Result<(), String> {
    let mut rows = source
        .query("PRAGMA quick_check", ())
        .await
        .map_err(|error| format!("source quick_check failed: {error}"))?;
    let result = rows
        .next()
        .await
        .map_err(|error| format!("source quick_check could not be read: {error}"))?
        .and_then(|row| row.get::<String>(0).ok())
        .unwrap_or_default();
    if result == "ok" {
        Ok(())
    } else {
        Err(format!("source quick_check reported: {result}"))
    }
}

async fn source_lcm_schema_version(source: &Connection) -> Result<i64, String> {
    if table_columns(source, "session_schema_migrations")
        .await?
        .is_empty()
    {
        return Ok(0);
    }
    let mut rows = source
        .query(
            "SELECT version FROM session_schema_migrations WHERE name = 'lcm'",
            (),
        )
        .await
        .map_err(|error| format!("could not inspect source schema: {error}"))?;
    match rows
        .next()
        .await
        .map_err(|error| format!("could not read source schema: {error}"))?
    {
        Some(row) => row
            .get(0)
            .map_err(|error| format!("invalid source schema version: {error}")),
        None => Ok(0),
    }
}

async fn ensure_message_identity_matches(
    source: &Connection,
    target: &Connection,
    table: &str,
    content_column: &str,
) -> Result<(), String> {
    let columns = table_columns(source, table).await?;
    if !["provider", "message_id", content_column]
        .iter()
        .all(|required| columns.iter().any(|column| column == required))
    {
        return Ok(());
    }
    let table = quote_identifier(table);
    let content_column = quote_identifier(content_column);
    let mut rows = source
        .query(
            &format!(
                "SELECT provider, message_id, {content_column} FROM {table} ORDER BY provider, message_id"
            ),
            (),
        )
        .await
        .map_err(|error| format!("could not inspect legacy message identities: {error}"))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| format!("could not read legacy message identity: {error}"))?
    {
        let provider = row
            .get::<String>(0)
            .map_err(|error| format!("invalid legacy message provider: {error}"))?;
        let message_id = row
            .get::<String>(1)
            .map_err(|error| format!("invalid legacy message id: {error}"))?;
        let source_content = row
            .get::<String>(2)
            .map_err(|error| format!("invalid legacy message content identity: {error}"))?;
        let mut target_rows = target
            .query(
                &format!(
                    "SELECT {content_column} FROM {table} WHERE provider = ?1 AND message_id = ?2"
                ),
                params![provider.as_str(), message_id.as_str()],
            )
            .await
            .map_err(|error| format!("could not inspect target message identity: {error}"))?;
        let Some(target_row) = target_rows
            .next()
            .await
            .map_err(|error| format!("could not read target message identity: {error}"))?
        else {
            continue;
        };
        let target_content = target_row
            .get::<String>(0)
            .map_err(|error| format!("invalid target message content identity: {error}"))?;
        if target_content != source_content {
            return Err(format!(
                "legacy {table} identity ({provider}, {message_id}) conflicts with target content"
            ));
        }
    }
    Ok(())
}

async fn resolve_target_project<R, F>(
    source: Option<&Connection>,
    config_path: &Path,
    user_home: &Path,
    hermes_homes: &[PathBuf],
    tracedecay_profile_root: &Path,
    registry_runtime: &R,
    read_pinned_project_root: &F,
) -> Result<ResolvedTargetProject, String>
where
    R: RegistryRuntime,
    F: Fn(&Path) -> Option<String>,
{
    let registry_path = tracedecay_profile_root.join("global.db");
    let registry = if registry_path.is_file() {
        Some(
            registry_runtime
                .open_read_only_at(&registry_path)
                .await
                .ok_or_else(|| {
                    format!(
                        "could not open project registry '{}' read-only",
                        registry_path.display()
                    )
                })?,
        )
    } else {
        None
    };

    if let Some(pin) = read_pinned_project_root(config_path) {
        return resolve_project_candidate(
            Path::new(&pin),
            user_home,
            hermes_homes,
            registry.as_ref(),
        )
        .await?
        .ok_or_else(|| format!("legacy project pin '{pin}' is not a resolvable code project"));
    }

    let source = source
        .ok_or_else(|| "legacy memory store has no project pin or session metadata".to_string())?;
    let columns = table_columns(source, "sessions").await?;
    if columns.is_empty() {
        return Err("source has no sessions table and no legacy project pin".to_string());
    }
    let path_expr = if columns.iter().any(|column| column == "project_path") {
        "project_path"
    } else {
        "NULL"
    };
    let key_expr = if columns.iter().any(|column| column == "project_key") {
        "project_key"
    } else {
        "NULL"
    };
    let metadata_expr = if columns.iter().any(|column| column == "metadata_json") {
        "metadata_json"
    } else {
        "NULL"
    };
    let sql = format!("SELECT {path_expr}, {key_expr}, {metadata_expr} FROM sessions");
    let mut rows = source
        .query(&sql, ())
        .await
        .map_err(|error| format!("could not read source project metadata: {error}"))?;
    let mut candidate_rows = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| format!("could not read source project metadata row: {error}"))?
    {
        let mut candidates = BTreeSet::new();
        for candidate in [row.get::<Option<String>>(0), row.get::<Option<String>>(1)]
            .into_iter()
            .flatten()
            .flatten()
        {
            candidates.insert(PathBuf::from(candidate));
        }
        let malformed_metadata = match row.get::<Option<String>>(2) {
            Ok(Some(metadata)) => {
                collect_metadata_project_candidates(&metadata, &mut candidates).is_err()
            }
            Ok(None) => false,
            Err(_) => true,
        };
        candidate_rows.push((candidates, malformed_metadata));
    }

    let mut targets: BTreeMap<String, ResolvedTargetProject> = BTreeMap::new();
    let mut has_projectless_evidence = false;
    let mut has_unresolved_project_evidence = false;
    for (candidates, malformed_metadata) in candidate_rows {
        let mut row_targets: BTreeMap<String, ResolvedTargetProject> = BTreeMap::new();
        let mut row_has_unresolved_project_evidence = malformed_metadata;
        for candidate in candidates {
            if is_projectless_candidate(&candidate, user_home, hermes_homes) {
                continue;
            }
            let resolved =
                resolve_project_candidate(&candidate, user_home, hermes_homes, registry.as_ref())
                    .await?;
            let Some(target) = resolved else {
                row_has_unresolved_project_evidence = true;
                continue;
            };
            let key = target_key(&target);
            if let Some(existing) = row_targets.get(&key)
                && !same_path(&existing.root, &target.root)
            {
                return Err(project_identity_collision(&key, existing, &target));
            }
            row_targets.insert(key, target);
        }
        if row_targets.len() > 1 {
            return Err(format!(
                "one source session maps to {} projects; refusing an ambiguous migration",
                row_targets.len()
            ));
        }
        if row_has_unresolved_project_evidence {
            has_unresolved_project_evidence = true;
        }
        if let Some((key, target)) = row_targets.into_iter().next() {
            if let Some(existing) = targets.get(&key)
                && !same_path(&existing.root, &target.root)
            {
                return Err(project_identity_collision(&key, existing, &target));
            }
            targets.insert(key, target);
        } else if !row_has_unresolved_project_evidence {
            has_projectless_evidence = true;
        }
    }
    match targets.len() {
        1 if !has_projectless_evidence && !has_unresolved_project_evidence => targets
            .into_values()
            .next()
            .ok_or_else(|| "resolved project target disappeared".to_string()),
        0 if !has_unresolved_project_evidence => Ok(ResolvedTargetProject {
            root: PathBuf::from("user"),
            registry_project_id: None,
            user_scope: true,
        }),
        0 => Err("no durable real project path exists in source session metadata".to_string()),
        1 => Err(
            "source session metadata mixes projectless or unresolved evidence with a project; refusing an ambiguous migration"
                .to_string(),
        ),
        count => Err(format!(
            "source session metadata maps to {count} projects; refusing an ambiguous migration"
        )),
    }
}

fn target_key(target: &ResolvedTargetProject) -> String {
    target
        .registry_project_id
        .clone()
        .unwrap_or_else(|| format!("path:{}", canonical_project_key(&target.root)))
}

fn project_identity_collision(
    key: &str,
    existing: &ResolvedTargetProject,
    target: &ResolvedTargetProject,
) -> String {
    format!(
        "registered project identity '{key}' maps to both '{}' and '{}'; refusing a collision",
        existing.root.display(),
        target.root.display()
    )
}

fn is_projectless_candidate(candidate: &Path, user_home: &Path, hermes_homes: &[PathBuf]) -> bool {
    if candidate.as_os_str().is_empty() || candidate == Path::new("user") {
        return true;
    }
    if same_path(candidate, user_home) {
        return true;
    }
    hermes_homes
        .iter()
        .any(|hermes_home| same_path(candidate, hermes_home))
}

fn collect_metadata_project_candidates(
    raw: &str,
    candidates: &mut BTreeSet<PathBuf>,
) -> Result<(), ()> {
    let metadata = serde_json::from_str::<serde_json::Value>(raw).map_err(|_| ())?;
    let metadata = metadata.as_object().ok_or(())?;
    for key in [
        "hermes_session_cwd",
        "hermes_session_worktree",
        "cwd",
        "worktree",
        "project_root",
    ] {
        if let Some(value) = metadata.get(key) {
            let path = value.as_str().ok_or(())?;
            candidates.insert(PathBuf::from(path));
        }
    }
    Ok(())
}

async fn resolve_project_candidate<D: RegistryDatabase>(
    candidate: &Path,
    user_home: &Path,
    hermes_homes: &[PathBuf],
    registry: Option<&D>,
) -> Result<Option<ResolvedTargetProject>, String> {
    if !candidate.is_absolute() {
        return Ok(None);
    }

    let canonical_candidate = canonicalize_with_missing_tail(candidate);
    let context = if let Some(registry) = registry {
        let direct = registry.project_registry_context_by_alias(candidate).await;
        match (direct, canonical_candidate.as_deref()) {
            (Some(context), _) => Some(context),
            (None, Some(canonical)) if canonical != candidate => {
                registry.project_registry_context_by_alias(canonical).await
            }
            _ => None,
        }
    } else {
        None
    };
    if let Some(context) = context {
        let mut registered_paths = vec![
            PathBuf::from(&context.project.display_root),
            PathBuf::from(&context.project.canonical_root),
        ];
        registered_paths.extend(
            context
                .aliases
                .iter()
                .map(|alias| PathBuf::from(&alias.alias_path)),
        );
        for registered_path in registered_paths {
            if let Some(root) = real_project_root(&registered_path, user_home, hermes_homes) {
                return Ok(Some(ResolvedTargetProject {
                    root,
                    registry_project_id: Some(context.project.project_id),
                    user_scope: false,
                }));
            }
        }
        return Err(format!(
            "registered project alias '{}' maps to '{}', but no durable current project root exists",
            candidate.display(),
            context.project.project_id
        ));
    }

    Ok(
        real_project_root(candidate, user_home, hermes_homes).map(|root| ResolvedTargetProject {
            root,
            registry_project_id: None,
            user_scope: false,
        }),
    )
}

fn real_project_root(
    candidate: &Path,
    user_home: &Path,
    hermes_homes: &[PathBuf],
) -> Option<PathBuf> {
    if !candidate.is_absolute() || !candidate.is_dir() {
        return None;
    }
    let canonical = candidate.canonicalize().ok()?;
    let canonical_user_home = user_home
        .canonicalize()
        .unwrap_or_else(|_| user_home.to_path_buf());
    let is_hermes_home = hermes_homes.iter().any(|hermes_home| {
        let canonical_hermes_home = hermes_home
            .canonicalize()
            .unwrap_or_else(|_| hermes_home.clone());
        canonical == canonical_hermes_home
    });
    if canonical == canonical_user_home || is_hermes_home {
        return None;
    }
    if let Some(git_root) = crate::worktree::git_worktree_root(&canonical) {
        return Some(git_root);
    }
    crate::config::has_project_database(&canonical).then_some(canonical)
}

fn same_path(left: &Path, right: &Path) -> bool {
    canonicalize_with_missing_tail(left).unwrap_or_else(|| left.to_path_buf())
        == canonicalize_with_missing_tail(right).unwrap_or_else(|| right.to_path_buf())
}

/// Canonicalizes the deepest existing ancestor and reattaches a missing tail.
/// This preserves OS aliases such as macOS `/var` -> `/private/var` even after
/// the final project directory was moved or a symlink alias was removed.
fn canonicalize_with_missing_tail(path: &Path) -> Option<PathBuf> {
    let mut ancestor = path;
    let mut tail = Vec::new();
    loop {
        if let Ok(mut canonical) = ancestor.canonicalize() {
            for component in tail.iter().rev() {
                canonical.push(component);
            }
            return Some(canonical);
        }
        tail.push(ancestor.file_name()?.to_os_string());
        ancestor = ancestor.parent()?;
    }
}

async fn logical_source_fingerprint(
    source: Option<&Connection>,
    source_path: &Path,
    memory_source: Option<(&Connection, &Path)>,
) -> Result<String, String> {
    let mut hash = Sha256::new();
    hash.update(b"tracedecay-hermes-legacy-session-store-v1\0");
    hash.update(
        source_path
            .canonicalize()
            .unwrap_or_else(|_| source_path.to_path_buf())
            .to_string_lossy()
            .as_bytes(),
    );
    if let Some(source) = source {
        hash_connection_tables(&mut hash, source, COPIED_TABLES).await?;
    }
    if let Some((memory, memory_path)) = memory_source {
        hash.update(b"\0memory_path\0");
        hash.update(
            memory_path
                .canonicalize()
                .unwrap_or_else(|_| memory_path.to_path_buf())
                .to_string_lossy()
                .as_bytes(),
        );
        hash_connection_tables(&mut hash, memory, COPIED_MEMORY_TABLES).await?;
    }
    Ok(hex::encode(hash.finalize()))
}

async fn hash_connection_tables(
    hash: &mut Sha256,
    source: &Connection,
    tables: &[&str],
) -> Result<(), String> {
    for table in tables {
        let columns = table_columns(source, table).await?;
        if columns.is_empty() {
            continue;
        }
        hash.update(b"\0table\0");
        hash.update(table.as_bytes());
        for column in &columns {
            hash.update(b"\0column\0");
            hash.update(column.as_bytes());
        }
        let select = columns
            .iter()
            .map(|column| quote_identifier(column))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT {select} FROM {} ORDER BY rowid",
            quote_identifier(table)
        );
        let mut rows = source
            .query(&sql, ())
            .await
            .map_err(|error| format!("could not fingerprint source table {table}: {error}"))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("could not fingerprint source row in {table}: {error}"))?
        {
            hash.update(b"\0row\0");
            for index in 0..columns.len() {
                let value = row.get::<Value>(index as i32).map_err(|error| {
                    format!("could not fingerprint source value in {table}: {error}")
                })?;
                hash_sqlite_value(hash, value);
            }
        }
    }
    Ok(())
}

fn hash_sqlite_value(hash: &mut Sha256, value: Value) {
    match value {
        Value::Null => hash.update(b"n"),
        Value::Integer(value) => {
            hash.update(b"i");
            hash.update(value.to_le_bytes());
        }
        Value::Real(value) => {
            hash.update(b"r");
            hash.update(value.to_bits().to_le_bytes());
        }
        Value::Text(value) => {
            hash.update(b"t");
            hash.update((value.len() as u64).to_le_bytes());
            hash.update(value.as_bytes());
        }
        Value::Blob(value) => {
            hash.update(b"b");
            hash.update((value.len() as u64).to_le_bytes());
            hash.update(value);
        }
    }
}

async fn merge_memory_snapshot(source: &Connection, target_path: &Path) -> Result<u64, String> {
    if table_columns(source, "memory_facts").await?.is_empty() {
        return Ok(0);
    }
    verify_source(source).await?;
    let authority =
        crate::db::DatabaseAuthority::for_runtime(target_path, "merge memory migration target")
            .map_err(|error| format!("could not authorize target memory store: {error}"))?;
    let (target, _) = if target_path.is_file() {
        Database::open(target_path, &authority).await
    } else {
        Database::initialize(target_path, &authority).await
    }
    .map_err(|error| format!("could not open target memory store: {error}"))?;
    target
        .conn()
        .execute("BEGIN IMMEDIATE", ())
        .await
        .map_err(|error| format!("could not begin target memory migration: {error}"))?;
    let result = copy_memory_tables(source, target.conn()).await;
    let rows_copied = match result {
        Ok(rows_copied) => {
            if let Err(error) = target.conn().execute("COMMIT", ()).await {
                let _ = target.conn().execute("ROLLBACK", ()).await;
                return Err(format!("could not commit target memory migration: {error}"));
            }
            rows_copied
        }
        Err(error) => {
            let _ = target.conn().execute("ROLLBACK", ()).await;
            return Err(error);
        }
    };
    MemoryStore::new(target.conn())
        .rebuild_all_banks()
        .await
        .map_err(|error| format!("could not rebuild migrated memory banks: {error}"))?;
    Ok(rows_copied)
}

async fn copy_memory_tables(source: &Connection, target: &Connection) -> Result<u64, String> {
    let (fact_rows, fact_ids) = copy_memory_facts(source, target).await?;
    let (entity_rows, entity_ids) = copy_memory_entities(source, target).await?;
    let association_rows =
        copy_memory_fact_entities(source, target, &fact_ids, &entity_ids).await?;
    let feedback_rows = copy_memory_feedback(source, target, &fact_ids).await?;
    Ok(fact_rows + entity_rows + association_rows + feedback_rows)
}

async fn copy_memory_facts(
    source: &Connection,
    target: &Connection,
) -> Result<(u64, HashMap<i64, i64>), String> {
    let source_columns = table_columns(source, "memory_facts").await?;
    let target_columns = table_columns(target, "memory_facts").await?;
    let columns = source_columns
        .into_iter()
        .filter(|column| column != "fact_id" && target_columns.contains(column))
        .collect::<Vec<_>>();
    let content_index = columns
        .iter()
        .position(|column| column == "content")
        .ok_or_else(|| "legacy memory facts have no content column".to_string())?;
    let quoted = columns
        .iter()
        .map(|column| quote_identifier(column))
        .collect::<Vec<_>>()
        .join(", ");
    let mut rows = source
        .query(
            &format!("SELECT fact_id, {quoted} FROM memory_facts ORDER BY fact_id"),
            (),
        )
        .await
        .map_err(|error| format!("could not read legacy memory facts: {error}"))?;
    let mut copied = 0;
    let mut fact_ids = HashMap::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| format!("could not read legacy memory fact: {error}"))?
    {
        let source_id = row
            .get::<i64>(0)
            .map_err(|error| format!("invalid legacy memory fact id: {error}"))?;
        let mut values = Vec::with_capacity(columns.len());
        for index in 0..columns.len() {
            values.push(
                row.get::<Value>((index + 1) as i32)
                    .map_err(|error| format!("could not decode legacy memory fact: {error}"))?,
            );
        }
        let content = match &values[content_index] {
            Value::Text(content) => content.clone(),
            _ => return Err("legacy memory fact content is not text".to_string()),
        };
        let fingerprint = sqlite_row_fingerprint(&columns, &values);
        let target_id = memory_fact_id_by_content(target, &content).await?;
        let target_id = if let Some(target_id) = target_id {
            copied +=
                merge_memory_fact_collision(target, target_id, &columns, &values, &fingerprint)
                    .await?;
            target_id
        } else {
            copied += insert_row_or_skip_exact(target, "memory_facts", &columns, &values).await?;
            let target_id = memory_fact_id_by_content(target, &content)
                .await?
                .ok_or_else(|| "migrated memory fact is absent from target".to_string())?;
            record_memory_fact_merge_marker(target, target_id, &columns, &values, &fingerprint)
                .await?;
            target_id
        };
        fact_ids.insert(source_id, target_id);
    }
    Ok((copied, fact_ids))
}

const LEGACY_FACT_MERGES_KEY: &str = "_tracedecay_legacy_hermes_merges";

async fn memory_fact_id_by_content(
    target: &Connection,
    content: &str,
) -> Result<Option<i64>, String> {
    let mut rows = target
        .query(
            "SELECT fact_id FROM memory_facts WHERE content = ?1",
            params![content],
        )
        .await
        .map_err(|error| format!("could not resolve migrated memory fact: {error}"))?;
    rows.next()
        .await
        .map_err(|error| format!("could not read migrated memory fact: {error}"))?
        .map(|row| {
            row.get(0)
                .map_err(|error| format!("invalid migrated memory fact id: {error}"))
        })
        .transpose()
}

async fn merge_memory_fact_collision(
    target: &Connection,
    target_id: i64,
    columns: &[String],
    values: &[Value],
    fingerprint: &str,
) -> Result<u64, String> {
    let mut rows = target
        .query(
            "SELECT category, tags, trust_score, retrieval_count, access_count,
                    helpful_count, unhelpful_count, created_at, updated_at,
                    last_retrieved_at, last_recalled_at, last_feedback_at,
                    source, metadata
             FROM memory_facts WHERE fact_id = ?1",
            params![target_id],
        )
        .await
        .map_err(|error| format!("could not read colliding memory fact: {error}"))?;
    let row = rows
        .next()
        .await
        .map_err(|error| format!("could not read colliding memory fact: {error}"))?
        .ok_or_else(|| format!("colliding memory fact {target_id} disappeared"))?;
    let target_category: String = row.get(0).map_err(|error| error.to_string())?;
    let target_tags: String = row.get(1).map_err(|error| error.to_string())?;
    let target_trust: f64 = row.get(2).map_err(|error| error.to_string())?;
    let target_retrieval: i64 = row.get(3).map_err(|error| error.to_string())?;
    let target_access: i64 = row.get(4).map_err(|error| error.to_string())?;
    let target_helpful: i64 = row.get(5).map_err(|error| error.to_string())?;
    let target_unhelpful: i64 = row.get(6).map_err(|error| error.to_string())?;
    let target_created: i64 = row.get(7).map_err(|error| error.to_string())?;
    let target_updated: i64 = row.get(8).map_err(|error| error.to_string())?;
    let target_last_retrieved: Option<i64> = row.get(9).map_err(|error| error.to_string())?;
    let target_last_recalled: Option<i64> = row.get(10).map_err(|error| error.to_string())?;
    let target_last_feedback: Option<i64> = row.get(11).map_err(|error| error.to_string())?;
    let target_source: String = row.get(12).map_err(|error| error.to_string())?;
    let target_metadata: String = row.get(13).map_err(|error| error.to_string())?;

    let (metadata, already_merged) = merged_fact_metadata(
        &target_metadata,
        source_text(columns, values, "metadata").unwrap_or("{}"),
        fingerprint,
        columns,
        values,
    );
    if already_merged {
        return Ok(0);
    }

    let source_helpful = source_integer(columns, values, "helpful_count").unwrap_or(0);
    let source_unhelpful = source_integer(columns, values, "unhelpful_count").unwrap_or(0);
    let target_weight = 1_i64.saturating_add(target_helpful.saturating_add(target_unhelpful));
    let source_weight = 1_i64.saturating_add(source_helpful.saturating_add(source_unhelpful));
    let source_trust = source_real(columns, values, "trust_score").unwrap_or(0.5);
    let trust = ((target_trust * target_weight as f64) + (source_trust * source_weight as f64))
        / target_weight.saturating_add(source_weight) as f64;
    let source_category = source_text(columns, values, "category").unwrap_or("general");
    let category = if target_category == "general" && source_category != "general" {
        source_category
    } else {
        &target_category
    };
    let source_label = source_text(columns, values, "source").unwrap_or("manual");
    let source_label = if target_source == "manual" && source_label != "manual" {
        source_label
    } else {
        &target_source
    };
    let tags = merge_json_string_arrays(
        &target_tags,
        source_text(columns, values, "tags").unwrap_or("[]"),
    );
    target
        .execute(
            "UPDATE memory_facts
             SET category = ?1, tags = ?2, trust_score = ?3,
                 retrieval_count = ?4, access_count = ?5,
                 helpful_count = ?6, unhelpful_count = ?7,
                 created_at = ?8, updated_at = ?9,
                 last_retrieved_at = ?10, last_recalled_at = ?11,
                 last_feedback_at = ?12, source = ?13, metadata = ?14
             WHERE fact_id = ?15",
            params![
                category,
                tags,
                trust.clamp(0.0, 1.0),
                target_retrieval.saturating_add(
                    source_integer(columns, values, "retrieval_count").unwrap_or(0)
                ),
                target_access
                    .saturating_add(source_integer(columns, values, "access_count").unwrap_or(0)),
                target_helpful.saturating_add(source_helpful),
                target_unhelpful.saturating_add(source_unhelpful),
                min_nonzero(
                    target_created,
                    source_integer(columns, values, "created_at").unwrap_or(0),
                ),
                target_updated.max(source_integer(columns, values, "updated_at").unwrap_or(0)),
                max_optional(
                    target_last_retrieved,
                    source_integer(columns, values, "last_retrieved_at"),
                ),
                max_optional(
                    target_last_recalled,
                    source_integer(columns, values, "last_recalled_at"),
                ),
                max_optional(
                    target_last_feedback,
                    source_integer(columns, values, "last_feedback_at"),
                ),
                source_label,
                metadata,
                target_id,
            ],
        )
        .await
        .map_err(|error| format!("could not merge colliding memory fact: {error}"))?;
    Ok(1)
}

async fn record_memory_fact_merge_marker(
    target: &Connection,
    target_id: i64,
    columns: &[String],
    values: &[Value],
    fingerprint: &str,
) -> Result<(), String> {
    let mut rows = target
        .query(
            "SELECT metadata FROM memory_facts WHERE fact_id = ?1",
            params![target_id],
        )
        .await
        .map_err(|error| format!("could not read migrated memory metadata: {error}"))?;
    let metadata: String = rows
        .next()
        .await
        .map_err(|error| format!("could not read migrated memory metadata: {error}"))?
        .ok_or_else(|| format!("migrated memory fact {target_id} disappeared"))?
        .get(0)
        .map_err(|error| format!("invalid migrated memory metadata: {error}"))?;
    let (metadata, _) = merged_fact_metadata(&metadata, "{}", fingerprint, columns, values);
    target
        .execute(
            "UPDATE memory_facts SET metadata = ?1 WHERE fact_id = ?2",
            params![metadata, target_id],
        )
        .await
        .map_err(|error| format!("could not record migrated memory fact source: {error}"))?;
    Ok(())
}

fn merged_fact_metadata(
    target_raw: &str,
    source_raw: &str,
    fingerprint: &str,
    columns: &[String],
    values: &[Value],
) -> (String, bool) {
    let mut target = serde_json::from_str::<serde_json::Value>(target_raw)
        .unwrap_or_else(|_| serde_json::json!({}));
    if !target.is_object() {
        target = serde_json::json!({"legacy_target_metadata": target});
    }
    let serde_json::Value::Object(target_object) = &mut target else {
        return ("{}".to_string(), false);
    };
    if let Ok(serde_json::Value::Object(source)) =
        serde_json::from_str::<serde_json::Value>(source_raw)
    {
        for (key, value) in source {
            target_object.entry(key).or_insert(value);
        }
    }
    let merges = target_object
        .entry(LEGACY_FACT_MERGES_KEY)
        .or_insert_with(|| serde_json::json!({}));
    if !merges.is_object() {
        *merges = serde_json::json!({});
    }
    let serde_json::Value::Object(merges) = merges else {
        return (target.to_string(), false);
    };
    if merges.contains_key(fingerprint) {
        return (target.to_string(), true);
    }
    merges.insert(
        fingerprint.to_string(),
        serde_json::json!({
            "category": source_text(columns, values, "category"),
            "source": source_text(columns, values, "source"),
            "trust_score": source_real(columns, values, "trust_score"),
        }),
    );
    (target.to_string(), false)
}

fn merge_json_string_arrays(target: &str, source: &str) -> String {
    let mut merged = serde_json::from_str::<Vec<String>>(target).unwrap_or_default();
    for value in serde_json::from_str::<Vec<String>>(source).unwrap_or_default() {
        if !merged.iter().any(|existing| existing == &value) {
            merged.push(value);
        }
    }
    serde_json::to_string(&merged).unwrap_or_else(|_| "[]".to_string())
}

fn source_integer(columns: &[String], values: &[Value], name: &str) -> Option<i64> {
    let value = values.get(columns.iter().position(|column| column == name)?)?;
    match value {
        Value::Integer(value) => Some(*value),
        _ => None,
    }
}

fn source_real(columns: &[String], values: &[Value], name: &str) -> Option<f64> {
    let value = values.get(columns.iter().position(|column| column == name)?)?;
    match value {
        Value::Real(value) => Some(*value),
        Value::Integer(value) => Some(*value as f64),
        _ => None,
    }
}

fn source_text<'a>(columns: &[String], values: &'a [Value], name: &str) -> Option<&'a str> {
    let value = values.get(columns.iter().position(|column| column == name)?)?;
    match value {
        Value::Text(value) => Some(value),
        _ => None,
    }
}

fn min_nonzero(left: i64, right: i64) -> i64 {
    match (left, right) {
        (0, value) | (value, 0) => value,
        _ => left.min(right),
    }
}

fn max_optional(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (left, right) => left.or(right),
    }
}

fn sqlite_row_fingerprint(columns: &[String], values: &[Value]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"tracedecay-hermes-memory-row-v1\0");
    for (column, value) in columns.iter().zip(values) {
        hash.update(column.as_bytes());
        hash.update([0]);
        hash_sqlite_value(&mut hash, value.clone());
    }
    hex::encode(hash.finalize())
}

async fn copy_memory_entities(
    source: &Connection,
    target: &Connection,
) -> Result<(u64, HashMap<i64, i64>), String> {
    let source_columns = table_columns(source, "memory_entities").await?;
    if source_columns.is_empty() {
        return Ok((0, HashMap::new()));
    }
    let target_columns = table_columns(target, "memory_entities").await?;
    let columns = source_columns
        .into_iter()
        .filter(|column| column != "entity_id" && target_columns.contains(column))
        .collect::<Vec<_>>();
    let normalized_index = columns
        .iter()
        .position(|column| column == "normalized_name")
        .ok_or_else(|| "legacy memory entities have no normalized_name column".to_string())?;
    let quoted = columns
        .iter()
        .map(|column| quote_identifier(column))
        .collect::<Vec<_>>()
        .join(", ");
    let mut rows = source
        .query(
            &format!("SELECT entity_id, {quoted} FROM memory_entities ORDER BY entity_id"),
            (),
        )
        .await
        .map_err(|error| format!("could not read legacy memory entities: {error}"))?;
    let mut inserted = 0;
    let mut entity_ids = HashMap::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| format!("could not read legacy memory entity: {error}"))?
    {
        let source_id = row
            .get::<i64>(0)
            .map_err(|error| format!("invalid legacy memory entity id: {error}"))?;
        let mut values = Vec::with_capacity(columns.len());
        for index in 0..columns.len() {
            values.push(
                row.get::<Value>((index + 1) as i32)
                    .map_err(|error| format!("could not decode legacy memory entity: {error}"))?,
            );
        }
        let normalized_name = match &values[normalized_index] {
            Value::Text(value) => value.clone(),
            _ => return Err("legacy normalized entity name is not text".to_string()),
        };
        inserted += insert_row_or_skip_exact(target, "memory_entities", &columns, &values).await?;
        let mut target_rows = target
            .query(
                "SELECT entity_id FROM memory_entities WHERE normalized_name = ?1",
                params![normalized_name],
            )
            .await
            .map_err(|error| format!("could not resolve migrated memory entity: {error}"))?;
        let target_id = target_rows
            .next()
            .await
            .map_err(|error| format!("could not read migrated memory entity: {error}"))?
            .ok_or_else(|| "migrated memory entity is absent from target".to_string())?
            .get(0)
            .map_err(|error| format!("invalid migrated memory entity id: {error}"))?;
        entity_ids.insert(source_id, target_id);
    }
    Ok((inserted, entity_ids))
}

async fn copy_memory_fact_entities(
    source: &Connection,
    target: &Connection,
    fact_ids: &HashMap<i64, i64>,
    entity_ids: &HashMap<i64, i64>,
) -> Result<u64, String> {
    if table_columns(source, "memory_fact_entities")
        .await?
        .is_empty()
    {
        return Ok(0);
    }
    let mut rows = source
        .query(
            "SELECT fact_id, entity_id FROM memory_fact_entities ORDER BY fact_id, entity_id",
            (),
        )
        .await
        .map_err(|error| format!("could not read legacy memory associations: {error}"))?;
    let mut inserted = 0;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| format!("could not read legacy memory association: {error}"))?
    {
        let source_fact_id = row
            .get::<i64>(0)
            .map_err(|error| format!("invalid legacy fact association: {error}"))?;
        let source_entity_id = row
            .get::<i64>(1)
            .map_err(|error| format!("invalid legacy entity association: {error}"))?;
        let target_fact_id = fact_ids.get(&source_fact_id).ok_or_else(|| {
            format!("legacy association references missing fact {source_fact_id}")
        })?;
        let target_entity_id = entity_ids.get(&source_entity_id).ok_or_else(|| {
            format!("legacy association references missing entity {source_entity_id}")
        })?;
        inserted += insert_row_or_skip_exact(
            target,
            "memory_fact_entities",
            &["fact_id".to_string(), "entity_id".to_string()],
            &[
                Value::Integer(*target_fact_id),
                Value::Integer(*target_entity_id),
            ],
        )
        .await?;
    }
    Ok(inserted)
}

async fn copy_memory_feedback(
    source: &Connection,
    target: &Connection,
    fact_ids: &HashMap<i64, i64>,
) -> Result<u64, String> {
    let source_columns = table_columns(source, "memory_feedback_events").await?;
    if source_columns.is_empty() {
        return Ok(0);
    }
    let target_columns = table_columns(target, "memory_feedback_events").await?;
    let columns = source_columns
        .into_iter()
        .filter(|column| {
            column != "event_id" && column != "fact_id" && target_columns.contains(column)
        })
        .collect::<Vec<_>>();
    let quoted = columns
        .iter()
        .map(|column| quote_identifier(column))
        .collect::<Vec<_>>()
        .join(", ");
    let select_sql =
        format!("SELECT fact_id, {quoted} FROM memory_feedback_events ORDER BY event_id");
    let mut target_columns_with_fact = vec!["fact_id".to_string()];
    target_columns_with_fact.extend(columns.iter().cloned());
    let target_quoted = target_columns_with_fact
        .iter()
        .map(|column| quote_identifier(column))
        .collect::<Vec<_>>()
        .join(", ");
    let placeholders = (1..=target_columns_with_fact.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let insert_sql =
        format!("INSERT INTO memory_feedback_events ({target_quoted}) VALUES ({placeholders})");
    let mut rows = source
        .query(&select_sql, ())
        .await
        .map_err(|error| format!("could not read legacy memory feedback: {error}"))?;
    let mut inserted = 0;
    let mut source_occurrences: HashMap<String, u64> = HashMap::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| format!("could not read legacy memory feedback row: {error}"))?
    {
        let source_fact_id = row
            .get::<i64>(0)
            .map_err(|error| format!("invalid legacy feedback fact id: {error}"))?;
        let target_fact_id = fact_ids
            .get(&source_fact_id)
            .ok_or_else(|| format!("legacy feedback references missing fact {source_fact_id}"))?;
        let mut values = Vec::with_capacity(columns.len() + 1);
        values.push(Value::Integer(*target_fact_id));
        for index in 0..columns.len() {
            values
                .push(row.get::<Value>((index + 1) as i32).map_err(|error| {
                    format!("could not decode legacy memory feedback: {error}")
                })?);
        }
        let signature = sqlite_row_fingerprint(&target_columns_with_fact, &values);
        let occurrence = source_occurrences.entry(signature).or_default();
        *occurrence = occurrence.saturating_add(1);
        if count_exact_rows(
            target,
            "memory_feedback_events",
            &target_columns_with_fact,
            &values,
        )
        .await?
            >= *occurrence
        {
            continue;
        }
        inserted += target
            .execute(
                &insert_sql,
                libsql::params_from_iter(values.iter().cloned()),
            )
            .await
            .map_err(|error| format!("could not copy legacy memory feedback: {error}"))?;
    }
    Ok(inserted)
}

async fn count_exact_rows(
    target: &Connection,
    table: &str,
    columns: &[String],
    values: &[Value],
) -> Result<u64, String> {
    let predicates = columns
        .iter()
        .enumerate()
        .map(|(index, column)| format!("{} IS ?{}", quote_identifier(column), index + 1))
        .collect::<Vec<_>>()
        .join(" AND ");
    let sql = format!(
        "SELECT COUNT(*) FROM {} WHERE {predicates}",
        quote_identifier(table)
    );
    let mut rows = target
        .query(&sql, libsql::params_from_iter(values.iter().cloned()))
        .await
        .map_err(|error| format!("could not count target {table} rows: {error}"))?;
    rows.next()
        .await
        .map_err(|error| format!("could not read target {table} row count: {error}"))?
        .ok_or_else(|| format!("target {table} row count is absent"))?
        .get::<i64>(0)
        .map(|count| count.max(0) as u64)
        .map_err(|error| format!("invalid target {table} row count: {error}"))
}

async fn copy_table<F>(
    source: &Connection,
    target: &Connection,
    table: &str,
    excluded: &[&str],
    mut transform: F,
) -> Result<u64, String>
where
    F: FnMut(&[String], &mut Vec<Value>) -> Result<(), String>,
{
    let source_columns = table_columns(source, table).await?;
    if source_columns.is_empty() {
        return Ok(0);
    }
    let target_columns = table_columns(target, table).await?;
    if target_columns.is_empty() {
        return Err(format!("target is missing required table {table}"));
    }
    let columns = source_columns
        .into_iter()
        .filter(|column| target_columns.contains(column) && !excluded.contains(&column.as_str()))
        .collect::<Vec<_>>();
    if columns.is_empty() {
        return Ok(0);
    }
    let quoted = columns
        .iter()
        .map(|column| quote_identifier(column))
        .collect::<Vec<_>>()
        .join(", ");
    let select_sql = format!(
        "SELECT {quoted} FROM {} ORDER BY rowid",
        quote_identifier(table)
    );
    let mut source_rows = source
        .query(&select_sql, ())
        .await
        .map_err(|error| format!("could not read source table {table}: {error}"))?;
    let mut inserted = 0;
    while let Some(row) = source_rows
        .next()
        .await
        .map_err(|error| format!("could not read source row from {table}: {error}"))?
    {
        let mut values = Vec::with_capacity(columns.len());
        for index in 0..columns.len() {
            values.push(
                row.get::<Value>(index as i32).map_err(|error| {
                    format!("could not decode source row from {table}: {error}")
                })?,
            );
        }
        transform(&columns, &mut values)?;
        inserted += insert_row_or_skip_exact(target, table, &columns, &values).await?;
    }
    Ok(inserted)
}

/// Exact duplicates are explicit idempotent skips. Any uniqueness collision
/// with different data is an error, never an `INSERT OR IGNORE` data loss.
async fn insert_row_or_skip_exact(
    target: &Connection,
    table: &str,
    columns: &[String],
    values: &[Value],
) -> Result<u64, String> {
    let predicates = columns
        .iter()
        .enumerate()
        .map(|(index, column)| format!("{} IS ?{}", quote_identifier(column), index + 1))
        .collect::<Vec<_>>()
        .join(" AND ");
    let exact_sql = format!(
        "SELECT 1 FROM {} WHERE {predicates} LIMIT 1",
        quote_identifier(table)
    );
    let mut exact = target
        .query(&exact_sql, libsql::params_from_iter(values.iter().cloned()))
        .await
        .map_err(|error| format!("could not check target {table} row: {error}"))?;
    if exact
        .next()
        .await
        .map_err(|error| format!("could not read target {table} row: {error}"))?
        .is_some()
    {
        return Ok(0);
    }

    let quoted = columns
        .iter()
        .map(|column| quote_identifier(column))
        .collect::<Vec<_>>()
        .join(", ");
    let placeholders = (1..=columns.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let insert_sql = format!(
        "INSERT INTO {} ({quoted}) VALUES ({placeholders})",
        quote_identifier(table)
    );
    target
        .execute(
            &insert_sql,
            libsql::params_from_iter(values.iter().cloned()),
        )
        .await
        .map_err(|error| {
            format!(
                "legacy {table} row collides with a different target row; migration was rolled back: {error}"
            )
        })
}

async fn copy_raw_messages(
    source: &Connection,
    target: &Connection,
) -> Result<(u64, HashMap<i64, i64>), String> {
    let source_columns = table_columns(source, "lcm_raw_messages").await?;
    if source_columns.is_empty() {
        return Ok((0, HashMap::new()));
    }
    if !source_columns.iter().any(|column| column == "store_id") {
        return Err("source lcm_raw_messages has no store_id".to_string());
    }
    let target_columns = table_columns(target, "lcm_raw_messages").await?;
    let columns = source_columns
        .into_iter()
        .filter(|column| column != "store_id" && target_columns.contains(column))
        .collect::<Vec<_>>();
    let provider_index = columns
        .iter()
        .position(|column| column == "provider")
        .ok_or_else(|| "source raw messages have no provider".to_string())?;
    let message_index = columns
        .iter()
        .position(|column| column == "message_id")
        .ok_or_else(|| "source raw messages have no message_id".to_string())?;
    let quoted = columns
        .iter()
        .map(|column| quote_identifier(column))
        .collect::<Vec<_>>()
        .join(", ");
    let select_sql = format!("SELECT store_id, {quoted} FROM lcm_raw_messages ORDER BY store_id");
    let mut rows = source
        .query(&select_sql, ())
        .await
        .map_err(|error| format!("could not read source raw messages: {error}"))?;
    let mut inserted = 0;
    let mut id_map = HashMap::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| format!("could not read source raw message: {error}"))?
    {
        let source_id: i64 = row
            .get(0)
            .map_err(|error| format!("invalid source raw store_id: {error}"))?;
        let provider: String = row
            .get((provider_index + 1) as i32)
            .map_err(|error| format!("invalid source raw provider: {error}"))?;
        let message_id: String = row
            .get((message_index + 1) as i32)
            .map_err(|error| format!("invalid source raw message_id: {error}"))?;
        let mut values = Vec::with_capacity(columns.len());
        for index in 0..columns.len() {
            values.push(
                row.get::<Value>((index + 1) as i32)
                    .map_err(|error| format!("could not decode source raw message: {error}"))?,
            );
        }
        inserted += insert_row_or_skip_exact(target, "lcm_raw_messages", &columns, &values).await?;
        let mut target_rows = target
            .query(
                "SELECT store_id FROM lcm_raw_messages WHERE provider = ?1 AND message_id = ?2",
                params![provider, message_id],
            )
            .await
            .map_err(|error| format!("could not resolve target raw store_id: {error}"))?;
        let target_id = target_rows
            .next()
            .await
            .map_err(|error| format!("could not read target raw store_id: {error}"))?
            .ok_or_else(|| "copied raw message is absent from target".to_string())?
            .get(0)
            .map_err(|error| format!("invalid target raw store_id: {error}"))?;
        id_map.insert(source_id, target_id);
    }
    Ok((inserted, id_map))
}

fn remap_summary_source(
    columns: &[String],
    values: &mut [Value],
    id_map: &HashMap<i64, i64>,
) -> Result<(), String> {
    let kind_index = columns
        .iter()
        .position(|column| column == "source_kind")
        .ok_or_else(|| "summary source has no source_kind".to_string())?;
    let id_index = columns
        .iter()
        .position(|column| column == "source_id")
        .ok_or_else(|| "summary source has no source_id".to_string())?;
    if matches!(&values[kind_index], Value::Text(kind) if kind == "raw_message") {
        let Value::Text(source_id) = &values[id_index] else {
            return Err("raw summary source has a non-text source_id".to_string());
        };
        let source_id = source_id
            .parse::<i64>()
            .map_err(|_| "raw summary source has an invalid store_id".to_string())?;
        let target_id = id_map
            .get(&source_id)
            .ok_or_else(|| format!("raw summary source {source_id} was not copied"))?;
        values[id_index] = Value::Text(target_id.to_string());
    }
    Ok(())
}

fn remap_store_id_columns(
    columns: &[String],
    values: &mut [Value],
    id_map: &HashMap<i64, i64>,
    remapped_columns: &[&str],
) -> Result<(), String> {
    for (column, value) in columns.iter().zip(values.iter_mut()) {
        if !remapped_columns.contains(&column.as_str()) {
            continue;
        }
        let Value::Integer(source_id) = value else {
            continue;
        };
        let target_id = id_map
            .get(source_id)
            .ok_or_else(|| format!("referenced raw store_id {source_id} was not copied"))?;
        *value = Value::Integer(*target_id);
    }
    Ok(())
}

async fn copy_external_payload_files(
    source: &Connection,
    source_db_path: &Path,
    target_db_path: &Path,
    created: &mut Vec<PathBuf>,
) -> Result<(), String> {
    if table_columns(source, "lcm_external_payloads")
        .await?
        .is_empty()
    {
        return Ok(());
    }
    let source_dir = source_db_path
        .parent()
        .ok_or_else(|| "source session DB has no parent directory".to_string())?
        .join("lcm-payloads");
    let target_dir = target_db_path
        .parent()
        .ok_or_else(|| "target session DB has no parent directory".to_string())?
        .join("lcm-payloads");
    let mut rows = source
        .query(
            "SELECT payload_ref, content_hash FROM lcm_external_payloads ORDER BY payload_ref",
            (),
        )
        .await
        .map_err(|error| format!("could not inspect source payloads: {error}"))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| format!("could not read source payload: {error}"))?
    {
        let payload_ref: String = row
            .get(0)
            .map_err(|error| format!("invalid source payload ref: {error}"))?;
        let expected_hash: String = row
            .get(1)
            .map_err(|error| format!("invalid source payload hash: {error}"))?;
        tracedecay_sessions::lcm::payload::validate_payload_ref(&payload_ref)
            .map_err(|error| format!("unsafe source payload ref '{payload_ref}': {error}"))?;
        let source_file = source_dir.join(&payload_ref);
        let metadata = fs::symlink_metadata(&source_file).map_err(|error| {
            format!(
                "source payload '{}' is unavailable: {error}",
                source_file.display()
            )
        })?;
        if !metadata.file_type().is_file() {
            return Err(format!(
                "source payload '{}' is not a regular file",
                source_file.display()
            ));
        }
        let bytes = fs::read(&source_file).map_err(|error| {
            format!(
                "could not read source payload '{}': {error}",
                source_file.display()
            )
        })?;
        let actual_hash = hex::encode(Sha256::digest(&bytes));
        if actual_hash != expected_hash {
            return Err(format!(
                "source payload '{}' failed its content hash",
                source_file.display()
            ));
        }
        fs::create_dir_all(&target_dir)
            .map_err(|error| format!("could not create target payload directory: {error}"))?;
        let target_metadata = fs::symlink_metadata(&target_dir)
            .map_err(|error| format!("could not inspect target payload directory: {error}"))?;
        if !target_metadata.file_type().is_dir() {
            return Err("target payload directory is not a regular directory".to_string());
        }
        let target_file = target_dir.join(&payload_ref);
        if target_file.exists() {
            let existing = fs::read(&target_file)
                .map_err(|error| format!("could not read existing target payload: {error}"))?;
            if hex::encode(Sha256::digest(&existing)) != expected_hash {
                return Err(format!(
                    "target payload '{}' conflicts with the legacy source",
                    target_file.display()
                ));
            }
            continue;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target_file)
            .map_err(|error| format!("could not create target payload: {error}"))?;
        created.push(target_file.clone());
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("could not persist target payload: {error}"))?;
    }
    Ok(())
}

fn remove_created_payloads(paths: &[PathBuf]) {
    for path in paths.iter().rev() {
        let _ = fs::remove_file(path);
    }
}

async fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>, String> {
    let sql = format!("PRAGMA table_info({})", quote_identifier(table));
    let mut rows = conn
        .query(&sql, ())
        .await
        .map_err(|error| format!("could not inspect table {table}: {error}"))?;
    let mut columns = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| format!("could not read table {table} columns: {error}"))?
    {
        columns.push(
            row.get(1)
                .map_err(|error| format!("invalid table {table} column: {error}"))?,
        );
    }
    Ok(columns)
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::hermes::HermesIntegration;
    use crate::agents::{AgentIntegration, InstallContext, UpdatePluginOutcome};
    use crate::memory::types::{AddFactRequest, FeedbackAction, FeedbackRequest, MemoryCategory};
    use crate::sessions::{SessionMessageRecord, SessionRecord};

    async fn test_initialize(path: &Path) -> (Database, bool) {
        let authority =
            crate::db::DatabaseAuthority::acquire_test(path, "Hermes migration test initialize")
                .unwrap();
        Database::initialize(path, &authority).await.unwrap()
    }

    async fn test_open(path: &Path) -> (Database, bool) {
        let authority =
            crate::db::DatabaseAuthority::acquire_test(path, "Hermes migration test open").unwrap();
        Database::open(path, &authority).await.unwrap()
    }

    async fn test_open_read_only(path: &Path) -> (Database, bool) {
        let authority =
            crate::db::DatabaseAuthority::acquire_test(path, "Hermes migration test read").unwrap();
        Database::open_read_only(path, &authority).await.unwrap()
    }

    fn mark_real_project(project: &Path) {
        fs::create_dir_all(project.join(".tracedecay")).unwrap();
        fs::write(project.join(".tracedecay/tracedecay.db"), []).unwrap();
    }

    #[tokio::test]
    async fn registry_cleanup_preserves_reassigned_project_identity() {
        let temp = tempfile::tempdir().unwrap();
        let profile_root = temp.path().join("profile");
        let legacy_root = temp.path().join("home/.hermes");
        let corrected_root = temp.path().join("projects/hermes-agent");
        fs::create_dir_all(&legacy_root).unwrap();
        fs::create_dir_all(&corrected_root).unwrap();
        let registry = GlobalDb::open_at(&profile_root.join("global.db"))
            .await
            .unwrap();
        registry
            .upsert_code_project("reassigned", &corrected_root, None, None, None)
            .await
            .unwrap();

        remove_legacy_registry_metadata(&profile_root, Some("reassigned"), &legacy_root)
            .await
            .unwrap();

        assert!(registry.get_code_project("reassigned").await.is_some());
    }

    async fn seed_source(path: &Path, sessions: &[(&str, &Path)]) {
        let db = GlobalDb::open_at(path).await.expect("open source");
        for (ordinal, (session_id, project)) in sessions.iter().enumerate() {
            let project = project.to_string_lossy().to_string();
            assert!(
                db.upsert_session(&SessionRecord {
                    provider: "hermes".into(),
                    session_id: (*session_id).into(),
                    project_key: project.clone(),
                    project_path: project,
                    title: Some("legacy".into()),
                    started_at: Some(ordinal as i64 + 1),
                    ended_at: None,
                    transcript_path: None,
                    metadata_json: None,
                    parent_session_id: None,
                    is_subagent: false,
                    agent_id: None,
                    parent_tool_use_id: None,
                })
                .await
            );
            assert!(
                db.upsert_session_message(&SessionMessageRecord {
                    provider: "hermes".into(),
                    message_id: format!("message-{session_id}"),
                    session_id: (*session_id).into(),
                    role: "user".into(),
                    timestamp: Some(ordinal as i64 + 1),
                    ordinal: 0,
                    text: "keep this".into(),
                    kind: None,
                    model: None,
                    tool_names: None,
                    source_path: None,
                    source_offset: None,
                    metadata_json: None,
                })
                .await
            );
        }
    }

    async fn seed_memory_fact(path: &Path, content: &str) -> i64 {
        let (db, _) = test_initialize(path).await;
        MemoryStore::new(db.conn())
            .add_fact(
                AddFactRequest {
                    content: content.to_string(),
                    category: MemoryCategory::Decision,
                    source: Some("hermes".to_string()),
                    tags: vec!["legacy".to_string()],
                    entities: vec!["TraceDecay".to_string()],
                    trust: Some(0.9),
                    metadata: serde_json::json!({"migration_test": true}),
                },
                0.5,
            )
            .await
            .unwrap()
            .fact
            .unwrap()
            .fact_id
    }

    async fn seed_legacy_state_db_without_cwd(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let db = libsql::Builder::new_local(path).build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                source TEXT NOT NULL,
                model TEXT,
                parent_session_id TEXT,
                started_at REAL NOT NULL,
                ended_at REAL,
                title TEXT,
                input_tokens INTEGER DEFAULT 0,
                output_tokens INTEGER DEFAULT 0,
                cache_read_tokens INTEGER DEFAULT 0,
                cache_write_tokens INTEGER DEFAULT 0,
                reasoning_tokens INTEGER DEFAULT 0
             );
             CREATE TABLE messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT,
                tool_calls TEXT,
                tool_name TEXT,
                timestamp REAL NOT NULL,
                reasoning TEXT,
                active INTEGER NOT NULL DEFAULT 1
             );
             INSERT INTO sessions (
                id, source, model, started_at, ended_at, title
             ) VALUES (
                'legacy-state-session', 'tui', 'legacy-model', 1.0, 2.0, 'legacy state'
             );
             INSERT INTO messages (
                session_id, role, content, timestamp
             ) VALUES (
                'legacy-state-session', 'user', 'state row without cwd', 1.0
             );",
        )
        .await
        .unwrap();
    }

    async fn count(conn: &Connection, table: &str) -> i64 {
        let mut rows = conn
            .query(&format!("SELECT COUNT(*) FROM {table}"), ())
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get(0).unwrap()
    }

    fn marker_count(target_db_path: &Path) -> usize {
        target_db_path
            .parent()
            .and_then(|root| fs::read_dir(root.join(LEDGER_DIR)).ok())
            .map(|entries| entries.flatten().count())
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn migrates_standard_profile_store_once() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let hermes = user_home.join(".hermes");
        let source = hermes.join(".tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(
            hermes.join("config.yaml"),
            format!(
                "plugins:\n  tracedecay:\n    project_root: {}\n",
                project.display()
            ),
        )
        .unwrap();
        seed_source(&source, &[("session-1", &project)]).await;
        seed_memory_fact(
            &source.with_file_name("tracedecay.db"),
            "legacy Hermes fact",
        )
        .await;

        let first = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(first.migrated.len(), 1, "{first:?}");
        assert!(first.migrated[0].rows_copied >= 3);
        let layout = crate::storage::resolve_layout(&project, &profile_root).unwrap();
        let target = GlobalDb::open_read_only_at(&layout.sessions_db_path)
            .await
            .unwrap();
        assert_eq!(count(target.conn(), "sessions").await, 1);
        assert_eq!(count(target.conn(), "session_messages").await, 1);
        assert_eq!(count(target.conn(), "lcm_raw_messages").await, 1);
        assert_eq!(marker_count(&layout.sessions_db_path), 1);
        let (target_code, _) = test_open_read_only(&layout.graph_db_path).await;
        let facts = MemoryStore::new(target_code.conn())
            .list_facts(None, None, 10)
            .await
            .unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].content, "legacy Hermes fact");
        assert!(facts[0].entities.contains(&"TraceDecay".to_string()));
        assert_eq!(
            target
                .get_session("hermes", "session-1")
                .await
                .unwrap()
                .project_path,
            GlobalDb::canonical_project_key(&project)
        );
        drop(target);

        let second = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(second.already_migrated.len(), 1, "{second:?}");
        let target = GlobalDb::open_read_only_at(&layout.sessions_db_path)
            .await
            .unwrap();
        assert_eq!(count(target.conn(), "sessions").await, 1);
        assert_eq!(marker_count(&layout.sessions_db_path), 1);
        let (target_code, _) = test_open_read_only(&layout.graph_db_path).await;
        assert_eq!(
            MemoryStore::new(target_code.conn())
                .list_facts(None, None, 10)
                .await
                .unwrap()
                .len(),
            1
        );
        let source_after = GlobalDb::open_read_only_at(&source).await.unwrap();
        assert_eq!(count(source_after.conn(), "sessions").await, 1);
    }

    #[tokio::test]
    async fn migration_marker_remerges_when_a_target_row_is_missing() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let hermes = user_home.join(".hermes");
        let source = hermes.join(".tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(
            hermes.join("config.yaml"),
            format!(
                "plugins:\n  tracedecay:\n    project_root: {}\n",
                project.display()
            ),
        )
        .unwrap();
        seed_source(&source, &[("session-1", &project)]).await;

        let first = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(first.migrated.len(), 1, "{first:?}");
        let initial_rows_copied = first.migrated[0].rows_copied;
        let layout = crate::storage::resolve_layout(&project, &profile_root).unwrap();
        let target = GlobalDb::open_at(&layout.sessions_db_path).await.unwrap();
        assert_eq!(
            target
                .conn()
                .execute(
                    "DELETE FROM session_messages WHERE provider = 'hermes' AND message_id = 'message-session-1'",
                    (),
                )
                .await
                .unwrap(),
            1
        );
        drop(target);

        let repaired = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(repaired.migrated.len(), 1, "{repaired:?}");
        assert!(repaired.already_migrated.is_empty(), "{repaired:?}");
        assert_eq!(repaired.migrated[0].rows_copied, 1, "{repaired:?}");
        let target = GlobalDb::open_read_only_at(&layout.sessions_db_path)
            .await
            .unwrap();
        assert_eq!(count(target.conn(), "session_messages").await, 1);
        drop(target);

        let verified = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(verified.already_migrated.len(), 1, "{verified:?}");
        assert_eq!(
            verified.already_migrated[0].rows_copied,
            initial_rows_copied + 1,
            "{verified:?}"
        );
        assert_eq!(marker_count(&layout.sessions_db_path), 1);

        let marker_path = fs::read_dir(layout.sessions_db_path.parent().unwrap().join(LEDGER_DIR))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let mut marker: serde_json::Value =
            serde_json::from_slice(&fs::read(&marker_path).unwrap()).unwrap();
        marker["schema_version"] = serde_json::json!(1);
        marker.as_object_mut().unwrap().remove("target_project_id");
        marker.as_object_mut().unwrap().remove("target_db_path");
        fs::write(&marker_path, serde_json::to_vec_pretty(&marker).unwrap()).unwrap();

        let upgraded = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(upgraded.already_migrated.len(), 1, "{upgraded:?}");
        let mut marker: serde_json::Value =
            serde_json::from_slice(&fs::read(&marker_path).unwrap()).unwrap();
        assert_eq!(marker["schema_version"], 2);
        assert!(marker["target_project_id"].as_str().is_some());
        assert!(marker["target_db_path"].as_str().is_some());

        marker["target_project_id"] = serde_json::json!("proj_wrong_target");
        fs::write(&marker_path, serde_json::to_vec_pretty(&marker).unwrap()).unwrap();

        let mismatched = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(mismatched.failed.len(), 1, "{mismatched:?}");
        assert!(
            mismatched.failed[0]
                .reason
                .contains("different project store")
        );
    }

    #[tokio::test]
    async fn migrates_pinned_memory_store_without_session_store() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let hermes = user_home.join(".hermes");
        fs::create_dir_all(hermes.join(".tracedecay")).unwrap();
        fs::write(
            hermes.join("config.yaml"),
            format!(
                "plugins:\n  tracedecay:\n    project_root: {}\n",
                project.display()
            ),
        )
        .unwrap();
        seed_memory_fact(
            &hermes.join(".tracedecay/tracedecay.db"),
            "facts survive without sessions",
        )
        .await;

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(report.migrated.len(), 1, "{report:?}");
        let layout = crate::storage::resolve_layout(&project, &profile_root).unwrap();
        let (target, _) = test_open_read_only(&layout.graph_db_path).await;
        let facts = MemoryStore::new(target.conn())
            .list_facts(None, None, 10)
            .await
            .unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].content, "facts survive without sessions");
    }

    #[tokio::test]
    async fn migrates_pinned_state_db_rows_without_cwd_before_unpin() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let hermes = user_home.join(".hermes");
        fs::create_dir_all(&hermes).unwrap();
        fs::write(
            hermes.join("config.yaml"),
            format!(
                "plugins:\n  tracedecay:\n    project_root: {}\n",
                project.display()
            ),
        )
        .unwrap();
        let state_db = hermes.join("state.db");
        seed_legacy_state_db_without_cwd(&state_db).await;

        let first = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(first.migrated.len(), 1, "{first:?}");
        assert_eq!(first.migrated[0].source_db, state_db);
        let layout = crate::storage::resolve_layout(&project, &profile_root).unwrap();
        let target = GlobalDb::open_read_only_at(&layout.sessions_db_path)
            .await
            .unwrap();
        assert_eq!(count(target.conn(), "sessions").await, 1);
        assert_eq!(count(target.conn(), "session_messages").await, 1);
        assert!(
            fs::read_to_string(hermes.join("config.yaml"))
                .unwrap()
                .contains("project_root"),
            "the migration layer must leave the pin for lifecycle cutover"
        );

        let second = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(second.already_migrated.len(), 1, "{second:?}");
        assert_eq!(count(target.conn(), "session_messages").await, 1);
    }

    #[tokio::test]
    async fn failed_state_db_import_preserves_project_pin() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let hermes = user_home.join(".hermes");
        fs::create_dir_all(&hermes).unwrap();
        let config = format!(
            "plugins:\n  tracedecay:\n    project_root: {}\n",
            project.display()
        );
        fs::write(hermes.join("config.yaml"), &config).unwrap();
        fs::write(hermes.join("state.db"), b"not sqlite").unwrap();

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;

        assert_eq!(report.failed.len(), 1, "{report:?}");
        assert_eq!(
            fs::read_to_string(hermes.join("config.yaml")).unwrap(),
            config
        );
    }

    #[tokio::test]
    async fn named_profile_upgrade_refreshes_in_place_without_default_cutover() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let legacy_profile = user_home.join(".hermes/profiles/work");
        let legacy_plugin = legacy_profile.join("plugins/tracedecay");
        fs::create_dir_all(&legacy_plugin).unwrap();
        fs::write(legacy_plugin.join("plugin.yaml"), "name: tracedecay\n").unwrap();
        let legacy_config = format!(
            "plugins:\n  enabled:\n    - tracedecay\n  tracedecay:\n    project_root: {}\n",
            project.display()
        );
        fs::write(legacy_profile.join("config.yaml"), &legacy_config).unwrap();
        seed_legacy_state_db_without_cwd(&legacy_profile.join("state.db")).await;

        let first = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(first.migrated.len(), 1, "{first:?}");

        let default_config = user_home.join(".hermes/config.yaml");
        fs::write(&default_config, "memory:\n  provider: other\n").unwrap();
        let ctx = InstallContext {
            home: user_home.clone(),
            tracedecay_bin: "/bin/tracedecay".to_string(),
            tool_permissions: crate::agents::expected_tool_perms(),
            project_root: None,
            dashboard: false,
        };
        let outcome = HermesIntegration.update_plugin(&ctx).unwrap();
        assert!(matches!(
            outcome,
            UpdatePluginOutcome::Refreshed(paths) if paths == vec![legacy_plugin.clone()]
        ));
        assert!(legacy_plugin.join("plugin.yaml").is_file());
        assert_eq!(
            fs::read_to_string(legacy_profile.join("config.yaml")).unwrap(),
            legacy_config
        );
        assert!(
            !user_home
                .join(".hermes/plugins/tracedecay/plugin.yaml")
                .exists()
        );

        let retry_migration = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(
            retry_migration.already_migrated.len(),
            1,
            "{retry_migration:?}"
        );
        fs::write(&default_config, "").unwrap();
        let outcome = HermesIntegration.update_plugin(&ctx).unwrap();
        assert!(matches!(
            outcome,
            UpdatePluginOutcome::Refreshed(paths) if paths == vec![legacy_plugin.clone()]
        ));
        assert!(legacy_plugin.join("plugin.yaml").is_file());
        assert!(
            !user_home
                .join(".hermes/plugins/tracedecay/plugin.yaml")
                .exists()
        );

        let layout = crate::storage::resolve_layout(&project, &profile_root).unwrap();
        let target = GlobalDb::open_read_only_at(&layout.sessions_db_path)
            .await
            .unwrap();
        assert_eq!(count(target.conn(), "session_messages").await, 1);
    }

    #[tokio::test]
    async fn same_content_memory_fact_merges_trust_and_feedback_once() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let hermes = user_home.join(".hermes");
        let source_sessions = hermes.join(".tracedecay/sessions.db");
        let source_memory = hermes.join(".tracedecay/tracedecay.db");
        fs::create_dir_all(source_sessions.parent().unwrap()).unwrap();
        fs::write(
            hermes.join("config.yaml"),
            format!(
                "plugins:\n  tracedecay:\n    project_root: {}\n",
                project.display()
            ),
        )
        .unwrap();
        seed_source(&source_sessions, &[("session", &project)]).await;
        let source_fact_id = seed_memory_fact(&source_memory, "shared durable fact").await;
        let (source_db, _) = test_open(&source_memory).await;
        MemoryStore::new(source_db.conn())
            .record_feedback_event(FeedbackRequest {
                fact_id: source_fact_id,
                action: FeedbackAction::Helpful,
                source: Some("legacy-hermes".to_string()),
                note: Some("source evidence".to_string()),
            })
            .await
            .unwrap();
        drop(source_db);

        let layout = crate::storage::resolve_layout(&project, &profile_root).unwrap();
        let (target_db, _) = test_initialize(&layout.graph_db_path).await;
        let target_store = MemoryStore::new(target_db.conn());
        let target_fact = target_store
            .add_fact(
                AddFactRequest {
                    content: "shared durable fact".to_string(),
                    category: MemoryCategory::Project,
                    source: Some("target".to_string()),
                    tags: vec!["target".to_string()],
                    entities: vec!["Target".to_string()],
                    trust: Some(0.2),
                    metadata: serde_json::json!({"target": true}),
                },
                0.5,
            )
            .await
            .unwrap()
            .fact
            .unwrap();
        target_store
            .record_feedback_event(FeedbackRequest {
                fact_id: target_fact.fact_id,
                action: FeedbackAction::Unhelpful,
                source: Some("target".to_string()),
                note: Some("target evidence".to_string()),
            })
            .await
            .unwrap();
        drop(target_db);

        let first = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(first.migrated.len(), 1, "{first:?}");
        let (target_db, _) = test_open_read_only(&layout.graph_db_path).await;
        let facts = MemoryStore::new(target_db.conn())
            .list_facts(None, None, 10)
            .await
            .unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].helpful_count, 1);
        assert_eq!(facts[0].unhelpful_count, 1);
        assert!(facts[0].tags.contains(&"legacy".to_string()));
        assert!(facts[0].tags.contains(&"target".to_string()));
        assert_eq!(count(target_db.conn(), "memory_feedback_events").await, 2);
        drop(target_db);

        let second = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(second.already_migrated.len(), 1, "{second:?}");
        let (target_db, _) = test_open_read_only(&layout.graph_db_path).await;
        assert_eq!(count(target_db.conn(), "memory_feedback_events").await, 2);
        let facts = MemoryStore::new(target_db.conn())
            .list_facts(None, None, 10)
            .await
            .unwrap();
        assert_eq!(facts[0].helpful_count, 1);
        assert_eq!(facts[0].unhelpful_count, 1);
    }

    #[tokio::test]
    async fn conflicting_existing_message_blocks_migration() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let hermes = user_home.join(".hermes");
        let source = hermes.join(".tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(
            hermes.join("config.yaml"),
            format!(
                "plugins:\n  tracedecay:\n    project_root: {}\n",
                project.display()
            ),
        )
        .unwrap();
        seed_source(&source, &[("session-1", &project)]).await;

        let layout = crate::storage::resolve_layout(&project, &profile_root).unwrap();
        seed_source(&layout.sessions_db_path, &[("session-1", &project)]).await;
        let target = GlobalDb::open_at(&layout.sessions_db_path).await.unwrap();
        assert!(
            target
                .upsert_session_message(&SessionMessageRecord {
                    provider: "hermes".into(),
                    message_id: "message-session-1".into(),
                    session_id: "session-1".into(),
                    role: "user".into(),
                    timestamp: Some(1),
                    ordinal: 0,
                    text: "conflicting target content".into(),
                    kind: None,
                    model: None,
                    tool_names: None,
                    source_path: None,
                    source_offset: None,
                    metadata_json: None,
                })
                .await
        );
        drop(target);

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(report.failed.len(), 1, "{report:?}");
        assert!(report.failed[0].reason.contains("conflicts"));
    }

    #[tokio::test]
    async fn nonidentical_session_identity_collision_is_reported() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let hermes = user_home.join(".hermes");
        let source = hermes.join(".tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(
            hermes.join("config.yaml"),
            format!(
                "plugins:\n  tracedecay:\n    project_root: {}\n",
                project.display()
            ),
        )
        .unwrap();
        seed_source(&source, &[("session-1", &project)]).await;

        let layout = crate::storage::resolve_layout(&project, &profile_root).unwrap();
        seed_source(&layout.sessions_db_path, &[("session-1", &project)]).await;
        let target = GlobalDb::open_at(&layout.sessions_db_path).await.unwrap();
        target
            .conn()
            .execute(
                "UPDATE sessions SET title = 'different target title'
                 WHERE provider = 'hermes' AND session_id = 'session-1'",
                (),
            )
            .await
            .unwrap();
        drop(target);

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;

        assert_eq!(report.failed.len(), 1, "{report:?}");
        assert!(report.failed[0].reason.contains("collides"));
        assert!(report.failed[0].reason.contains("sessions"));
    }

    #[tokio::test]
    async fn ambiguous_metadata_is_preserved_and_reported() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        mark_real_project(&first);
        mark_real_project(&second);
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        seed_source(&source, &[("first", &first), ("second", &second)]).await;

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(report.unresolved.len(), 1, "{report:?}");
        assert!(report.unresolved[0].reason.contains("ambiguous"));
        let source_after = GlobalDb::open_read_only_at(&source).await.unwrap();
        assert_eq!(count(source_after.conn(), "sessions").await, 2);
        assert!(
            !crate::storage::resolve_layout(&first, &profile_root)
                .unwrap()
                .sessions_db_path
                .exists()
        );
    }

    #[tokio::test]
    async fn one_unpinned_metadata_project_is_migrated() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        seed_source(&source, &[("session", &project)]).await;

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(report.migrated.len(), 1, "{report:?}");
        assert_eq!(
            report.migrated[0].target_project,
            project.canonicalize().unwrap()
        );
    }

    #[tokio::test]
    async fn moved_pinned_project_resolves_through_registered_alias() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let legacy_project = temp.path().join("project-before-move");
        let current_project = temp.path().join("project-after-move");
        mark_real_project(&legacy_project);
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(
            user_home.join(".hermes/config.yaml"),
            format!(
                "plugins:\n  tracedecay:\n    project_root: {}\n",
                legacy_project.display()
            ),
        )
        .unwrap();
        seed_source(&source, &[("session", &legacy_project)]).await;

        fs::create_dir_all(&profile_root).unwrap();
        let registry = GlobalDb::open_at(&profile_root.join("global.db"))
            .await
            .unwrap();
        registry
            .upsert_code_project("stable-project", &legacy_project, None, None, None)
            .await
            .unwrap();
        fs::rename(&legacy_project, &current_project).unwrap();
        registry
            .upsert_code_project("stable-project", &current_project, None, None, None)
            .await
            .unwrap();
        drop(registry);

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;

        assert_eq!(report.migrated.len(), 1, "{report:?}");
        assert_eq!(
            report.migrated[0].target_project,
            current_project.canonicalize().unwrap()
        );
        let target =
            GlobalDb::open_read_only_at(&profile_root.join("projects/stable-project/sessions.db"))
                .await
                .unwrap();
        assert_eq!(count(target.conn(), "sessions").await, 1);
        assert!(
            !crate::storage::resolve_layout(&current_project, &profile_root)
                .unwrap()
                .sessions_db_path
                .exists(),
            "migration must not create a second path-hash shard"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn moved_project_resolves_through_canonicalized_missing_parent_alias() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let physical_parent = temp.path().join("physical");
        let alias_parent = temp.path().join("alias");
        fs::create_dir_all(&physical_parent).unwrap();
        std::os::unix::fs::symlink(&physical_parent, &alias_parent).unwrap();
        let legacy_alias = alias_parent.join("project-before-move");
        let legacy_physical = physical_parent.join("project-before-move");
        let current_project = physical_parent.join("project-after-move");
        mark_real_project(&legacy_alias);
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(
            user_home.join(".hermes/config.yaml"),
            format!(
                "plugins:\n  tracedecay:\n    project_root: {}\n",
                legacy_alias.display()
            ),
        )
        .unwrap();
        seed_source(&source, &[("session", &legacy_alias)]).await;

        fs::create_dir_all(&profile_root).unwrap();
        let registry = GlobalDb::open_at(&profile_root.join("global.db"))
            .await
            .unwrap();
        registry
            .upsert_code_project("stable-project", &legacy_physical, None, None, None)
            .await
            .unwrap();
        fs::rename(&legacy_physical, &current_project).unwrap();
        registry
            .upsert_code_project("stable-project", &current_project, None, None, None)
            .await
            .unwrap();
        drop(registry);

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;

        assert_eq!(report.migrated.len(), 1, "{report:?}");
        assert_eq!(
            report.migrated[0].target_project,
            current_project.canonicalize().unwrap()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn removed_unprovable_symlink_metadata_is_preserved() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let legacy_project = temp.path().join("project-before-move");
        let project_alias = temp.path().join("project-link");
        let current_project = temp.path().join("project-after-move");
        mark_real_project(&legacy_project);
        std::os::unix::fs::symlink(&legacy_project, &project_alias).unwrap();
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        seed_source(&source, &[("session", &legacy_project)]).await;
        let source_rw = GlobalDb::open_at(&source).await.unwrap();
        source_rw
            .conn()
            .execute(
                "UPDATE sessions SET project_path = ?1 WHERE session_id = 'session'",
                [project_alias.to_string_lossy().to_string()],
            )
            .await
            .unwrap();
        drop(source_rw);

        fs::create_dir_all(&profile_root).unwrap();
        let registry = GlobalDb::open_at(&profile_root.join("global.db"))
            .await
            .unwrap();
        registry
            .upsert_code_project("stable-project", &project_alias, None, None, None)
            .await
            .unwrap();
        fs::remove_file(&project_alias).unwrap();
        fs::rename(&legacy_project, &current_project).unwrap();
        registry
            .upsert_code_project("stable-project", &current_project, None, None, None)
            .await
            .unwrap();
        drop(registry);

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;

        assert!(report.migrated.is_empty(), "{report:?}");
        assert_eq!(report.unresolved.len(), 1, "{report:?}");
        assert!(
            !profile_root
                .join("projects/stable-project/sessions.db")
                .is_file()
        );
    }

    #[tokio::test]
    async fn migrates_profile_shard_misidentified_as_hermes_project() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let hermes = user_home.join(".hermes");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let legacy_shard = profile_root.join("projects/legacy-hermes-identity");
        let source = legacy_shard.join(crate::storage::SESSIONS_DB_FILENAME);
        fs::create_dir_all(&legacy_shard).unwrap();
        let manifest = crate::storage::StoreManifest {
            schema_version: crate::storage::STORE_MANIFEST_SCHEMA_VERSION,
            project_id: Some("legacy-hermes-identity".into()),
            store_kind: crate::storage::StoreKind::CodeProject,
            storage_mode: crate::storage::StorageMode::ProfileSharded,
            project_root: hermes.clone(),
            data_root: legacy_shard.clone(),
            graph_db_relpath: PathBuf::from("tracedecay.db"),
            sessions_db_relpath: PathBuf::from(crate::storage::SESSIONS_DB_FILENAME),
            branch_meta_relpath: PathBuf::from(crate::storage::BRANCH_META_FILENAME),
        };
        fs::write(
            legacy_shard.join(crate::storage::STORE_MANIFEST_FILENAME),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let registry = GlobalDb::open_at(&profile_root.join("global.db"))
            .await
            .unwrap();
        registry
            .upsert_code_project("legacy-hermes-identity", &hermes, None, None, None)
            .await
            .unwrap();
        seed_source(&source, &[("session", &project)]).await;

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(report.migrated.len(), 1, "{report:?}");
        assert_eq!(report.migrated[0].source_db, source);
        let source_after = GlobalDb::open_read_only_at(&source).await.unwrap();
        assert_eq!(count(source_after.conn(), "sessions").await, 1);
        let target_layout = crate::storage::resolve_layout(&project, &profile_root).unwrap();
        assert_ne!(target_layout.sessions_db_path, source);
        let target = GlobalDb::open_read_only_at(&target_layout.sessions_db_path)
            .await
            .unwrap();
        assert_eq!(count(target.conn(), "sessions").await, 1);
        assert!(source.is_file());
        assert!(
            registry
                .get_code_project("legacy-hermes-identity")
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn migrates_hermes_owned_profile_shard_sessions_to_user_and_cleans_registry() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let hermes = user_home.join(".hermes");
        let legacy_shard = profile_root.join("projects/legacy-hermes-projectless");
        let source = legacy_shard.join(crate::storage::SESSIONS_DB_FILENAME);
        fs::create_dir_all(&legacy_shard).unwrap();
        let manifest = crate::storage::StoreManifest {
            schema_version: crate::storage::STORE_MANIFEST_SCHEMA_VERSION,
            project_id: Some("legacy-hermes-projectless".into()),
            store_kind: crate::storage::StoreKind::CodeProject,
            storage_mode: crate::storage::StorageMode::ProfileSharded,
            project_root: hermes.clone(),
            data_root: legacy_shard.clone(),
            graph_db_relpath: PathBuf::from("tracedecay.db"),
            sessions_db_relpath: PathBuf::from(crate::storage::SESSIONS_DB_FILENAME),
            branch_meta_relpath: PathBuf::from(crate::storage::BRANCH_META_FILENAME),
        };
        fs::write(
            legacy_shard.join(crate::storage::STORE_MANIFEST_FILENAME),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let registry = GlobalDb::open_at(&profile_root.join("global.db"))
            .await
            .unwrap();
        registry
            .upsert_code_project("legacy-hermes-projectless", &hermes, None, None, None)
            .await
            .unwrap();
        seed_source(&source, &[("session", &hermes)]).await;

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(report.migrated.len(), 1, "{report:?}");
        assert!(report.failed.is_empty(), "{report:?}");
        assert_eq!(report.migrated[0].source_db, source);
        assert_eq!(report.migrated[0].target_project, Path::new("user"));
        let target_path = crate::sessions::user_sessions_db_path(&profile_root);
        let target = GlobalDb::open_read_only_at(&target_path).await.unwrap();
        let session = target.get_session("hermes", "session").await.unwrap();
        assert_eq!(session.project_key, "user");
        assert_eq!(session.project_path, "user");
        let source_after = GlobalDb::open_read_only_at(&source).await.unwrap();
        for table in ["sessions", "session_messages"] {
            assert_eq!(
                count(source_after.conn(), table).await,
                count(target.conn(), table).await,
                "row parity for {table}"
            );
        }
        assert_eq!(marker_count(&target_path), 1);
        assert!(source.is_file());
        assert!(
            registry
                .get_code_project("legacy-hermes-projectless")
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn migrates_older_source_with_missing_current_columns_and_lcm_tables() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        let source_handle = libsql::Builder::new_local(&source).build().await.unwrap();
        let source_conn = source_handle.connect().unwrap();
        source_conn
            .execute_batch(
                "CREATE TABLE sessions (
                    provider TEXT NOT NULL,
                    session_id TEXT NOT NULL,
                    project_key TEXT NOT NULL,
                    project_path TEXT NOT NULL,
                    title TEXT,
                    PRIMARY KEY(provider, session_id)
                 );
                 CREATE TABLE session_messages (
                    provider TEXT NOT NULL,
                    message_id TEXT NOT NULL,
                    session_id TEXT NOT NULL,
                    role TEXT NOT NULL,
                    ordinal INTEGER NOT NULL,
                    text TEXT NOT NULL,
                    PRIMARY KEY(provider, message_id)
                 );",
            )
            .await
            .unwrap();
        let project_text = project.to_string_lossy().to_string();
        source_conn
            .execute(
                "INSERT INTO sessions(provider, session_id, project_key, project_path, title)
                 VALUES ('hermes', 'old-session', ?1, ?1, 'old')",
                [project_text],
            )
            .await
            .unwrap();
        source_conn
            .execute(
                "INSERT INTO session_messages(provider, message_id, session_id, role, ordinal, text)
                 VALUES ('hermes', 'old-message', 'old-session', 'user', 0, 'old text')",
                (),
            )
            .await
            .unwrap();
        drop(source_conn);
        drop(source_handle);

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(report.migrated.len(), 1, "{report:?}");
        let layout = crate::storage::resolve_layout(&project, &profile_root).unwrap();
        let target = GlobalDb::open_read_only_at(&layout.sessions_db_path)
            .await
            .unwrap();
        assert_eq!(count(target.conn(), "sessions").await, 1);
        assert_eq!(count(target.conn(), "session_messages").await, 1);
        assert_eq!(count(target.conn(), "lcm_raw_messages").await, 0);
    }

    #[tokio::test]
    async fn projectless_profile_sessions_migrate_to_user_store_idempotently() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let hermes = user_home.join(".hermes");
        let source = hermes.join(".tracedecay/sessions.db");
        let source_memory = hermes.join(".tracedecay/tracedecay.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        let source_fact_id = seed_memory_fact(&source_memory, "unscoped legacy fact").await;
        seed_source(&source, &[("session", &hermes)]).await;

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(report.migrated.len(), 1, "{report:?}");
        assert_eq!(report.unresolved.len(), 1, "{report:?}");
        assert_eq!(report.unresolved[0].source_db, source_memory);
        assert!(report.unresolved[0].reason.contains("preserved"));
        let target_path = crate::sessions::user_sessions_db_path(&profile_root);
        let target = GlobalDb::open_read_only_at(&target_path).await.unwrap();
        let session = target.get_session("hermes", "session").await.unwrap();
        assert_eq!(session.project_path, "user");
        assert!(!crate::memory::user::user_memory_db_path(&profile_root).exists());
        let source_after = GlobalDb::open_read_only_at(&source).await.unwrap();
        assert_eq!(count(source_after.conn(), "sessions").await, 1);
        drop(source_after);
        let (source_memory_after, _) = test_open_read_only(&source_memory).await;
        assert!(
            MemoryStore::new(source_memory_after.conn())
                .get_fact(source_fact_id)
                .await
                .unwrap()
                .is_some()
        );

        let retry = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(retry.already_migrated.len(), 1, "{retry:?}");
        assert_eq!(retry.unresolved.len(), 1, "{retry:?}");
        assert_eq!(count(target.conn(), "sessions").await, 1);
    }

    #[tokio::test]
    async fn malformed_metadata_is_preserved_not_misrouted_to_user() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        seed_source(&source, &[("session", &user_home)]).await;
        let source_rw = GlobalDb::open_at(&source).await.unwrap();
        source_rw
            .conn()
            .execute(
                "UPDATE sessions SET project_key = '', project_path = '', metadata_json = '{invalid' WHERE session_id = 'session'",
                (),
            )
            .await
            .unwrap();
        drop(source_rw);

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;

        assert_eq!(report.unresolved.len(), 1, "{report:?}");
        assert!(report.migrated.is_empty());
        assert!(!crate::sessions::user_sessions_db_path(&profile_root).exists());
    }

    #[tokio::test]
    async fn structurally_invalid_metadata_is_preserved_not_misrouted_to_user() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        seed_source(&source, &[("session", &user_home)]).await;
        let source_rw = GlobalDb::open_at(&source).await.unwrap();
        source_rw
            .conn()
            .execute(
                "UPDATE sessions SET project_key = '', project_path = '', metadata_json = '{\"project_root\":42}' WHERE session_id = 'session'",
                (),
            )
            .await
            .unwrap();
        drop(source_rw);

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;

        assert_eq!(report.unresolved.len(), 1, "{report:?}");
        assert!(report.migrated.is_empty());
        assert!(!crate::sessions::user_sessions_db_path(&profile_root).exists());
    }

    #[tokio::test]
    async fn vanished_hermes_owned_path_is_preserved_as_unresolved() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let vanished_project = user_home.join(".hermes/plugins/vanished-project");
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        seed_source(&source, &[("session", &vanished_project)]).await;

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert!(report.migrated.is_empty(), "{report:?}");
        assert_eq!(report.unresolved.len(), 1, "{report:?}");
        assert!(!crate::sessions::user_sessions_db_path(&profile_root).exists());
        let source_after = GlobalDb::open_read_only_at(&source).await.unwrap();
        assert_eq!(count(source_after.conn(), "sessions").await, 1);
    }

    #[tokio::test]
    async fn durable_project_under_hermes_home_remains_project_scoped() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = user_home.join(".hermes/workspaces/real-project");
        mark_real_project(&project);
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        seed_source(&source, &[("session", &project)]).await;

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(report.migrated.len(), 1, "{report:?}");
        assert!(report.unresolved.is_empty(), "{report:?}");
        assert_eq!(
            report.migrated[0].target_project,
            project.canonicalize().unwrap()
        );
        assert!(!crate::sessions::user_sessions_db_path(&profile_root).exists());
    }

    #[tokio::test]
    async fn existing_unregistered_directory_is_not_assumed_projectless() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let unregistered = temp.path().join("unregistered-project");
        fs::create_dir_all(&unregistered).unwrap();
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        seed_source(&source, &[("session", &unregistered)]).await;

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(report.unresolved.len(), 1, "{report:?}");
        assert!(report.migrated.is_empty());
        assert!(!crate::sessions::user_sessions_db_path(&profile_root).exists());
    }

    #[tokio::test]
    async fn same_session_resolved_and_unresolved_projects_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        let vanished = temp.path().join("vanished-project");
        mark_real_project(&project);
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        seed_source(&source, &[("session", &project)]).await;
        let source_rw = GlobalDb::open_at(&source).await.unwrap();
        source_rw
            .conn()
            .execute(
                "UPDATE sessions SET project_path = ?1 WHERE session_id = 'session'",
                [vanished.to_string_lossy().to_string()],
            )
            .await
            .unwrap();
        drop(source_rw);

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;

        assert_eq!(report.unresolved.len(), 1, "{report:?}");
        assert!(report.migrated.is_empty());
        let layout = crate::storage::resolve_layout(&project, &profile_root).unwrap();
        assert!(!layout.sessions_db_path.exists());
    }

    #[tokio::test]
    async fn mixed_user_and_project_sessions_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        seed_source(
            &source,
            &[("user-session", &user_home), ("project-session", &project)],
        )
        .await;

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(report.unresolved.len(), 1, "{report:?}");
        assert!(report.unresolved[0].reason.contains("ambiguous"));
        assert!(!crate::sessions::user_sessions_db_path(&profile_root).exists());
        let project_layout = crate::storage::resolve_layout(&project, &profile_root).unwrap();
        assert!(!project_layout.sessions_db_path.exists());
    }

    #[tokio::test]
    async fn future_source_schema_is_rejected_without_target_changes() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        seed_source(&source, &[("session", &project)]).await;
        let source_rw = GlobalDb::open_at(&source).await.unwrap();
        source_rw
            .conn()
            .execute(
                "UPDATE session_schema_migrations SET version = ?1 WHERE name = 'lcm'",
                [crate::sessions::lcm::LCM_SCHEMA_VERSION + 1],
            )
            .await
            .unwrap();
        drop(source_rw);

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(report.failed.len(), 1, "{report:?}");
        assert!(report.failed[0].reason.contains("newer"));
        assert!(
            !crate::storage::resolve_layout(&project, &profile_root)
                .unwrap()
                .sessions_db_path
                .exists()
        );
    }

    #[tokio::test]
    async fn injected_failure_rolls_back_and_retry_converges() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        seed_source(&source, &[("session", &project)]).await;
        let layout = crate::storage::resolve_layout(&project, &profile_root).unwrap();
        let target = GlobalDb::open_at(&layout.sessions_db_path).await.unwrap();
        assert_eq!(count(target.conn(), "sessions").await, 0);
        drop(target);

        let failed = migrate_legacy_hermes_stores_inner(
            &user_home,
            &profile_root,
            &[user_home.join(".hermes")],
            Some("sessions"),
        )
        .await;
        assert_eq!(failed.failed.len(), 1, "{failed:?}");
        let target = GlobalDb::open_read_only_at(&layout.sessions_db_path)
            .await
            .unwrap();
        assert_eq!(count(target.conn(), "sessions").await, 0);
        assert_eq!(marker_count(&layout.sessions_db_path), 0);
        drop(target);
        let source_after = GlobalDb::open_read_only_at(&source).await.unwrap();
        assert_eq!(count(source_after.conn(), "sessions").await, 1);
        drop(source_after);

        let retry = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(retry.migrated.len(), 1, "{retry:?}");
    }

    #[test]
    fn single_legacy_home_profile_scan_is_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let standard = temp.path().join("home/.hermes");
        fs::create_dir_all(standard.join("profiles/alpha")).unwrap();
        let profiles = legacy_profile_dirs(&standard);
        assert_eq!(
            profiles,
            vec![standard.clone(), standard.join("profiles/alpha")]
        );
    }
}
