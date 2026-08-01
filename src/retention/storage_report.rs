//! Read-only, cheap-to-query storage observability (plan 38 §7): per-store
//! size and free-page ratio for every registered profile-sharded store under
//! a profile root, plus an unregistered-directory backlog summary —
//! reachable from `tracedecay migrate storage-report` without a live daemon
//! or any [`crate::global_db::RegisteredGlobalDb`] writer authority.
//!
//! # Why this does not snapshot stores in place
//!
//! The registry read does, because it is one small file and needs a
//! consistent view of `code_projects`. The *per-store size sampling*
//! deliberately does not. [`crate::sqlite_read_snapshot`] freezes a database
//! family by reflinking it, falling back to a **full byte copy** when the
//! filesystem cannot reflink, and any graph database a live daemon has open
//! is WAL-backed and therefore takes that path. Running this report over the
//! profile it was written for — the owner's, at 91GB — would have copied
//! every registered store to read three pragmas off it, on a command whose
//! entire promise is being cheap enough to run on a full profile.
//!
//! So sizes come from filesystem metadata (exact, no locks, no I/O beyond
//! `stat`) and free-page counts from a short read-only connection. The one
//! registry snapshot uses an OS-temporary scratch directory, never a child of
//! the profile it inspects. If a free-page connection fails — busy, corrupt,
//! or a WAL database with no `-shm` to map read-only — its fields are `None`
//! rather than guessed at, and the store still reports its size.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use tracedecay_runtime_core::sqlite_read_snapshot::{
    BOUNDED_PROBE_BUSY_TIMEOUT, open_read_only_probe, pragma_u64,
};

use super::code_index_generations::{
    CodeGenerationRetentionGenerationV1, DEFAULT_SUPERSEDED_GENERATION_FLOOR,
    GenerationDigestVerificationV1, plan_code_generation_retention_with_verification,
    scoped_code_index_store_root,
};

const GLOBAL_DB_FILENAME: &str = "global.db";
const PROJECT_CURSOR_PREFIX: &str = "projects:";
const DIRECTORY_CURSOR_PREFIX: &str = "directories:";
pub const MAX_STORAGE_REPORT_PAGE_LIMIT: usize = 64;
const CODE_GENERATION_RETENTION_DIGEST_SCAN_MAX_BYTES: u64 = 32 * 1024 * 1024;
const CODE_GENERATIONS_DIRECTORY: &str = "code-generations-v1";

/// One registered profile-sharded store's size/free-page snapshot.
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct StoreSizeReportEntry {
    pub project_id: String,
    pub canonical_root: String,
    /// On-disk bytes of the graph database family (main file plus `-wal` and
    /// `-shm`), from filesystem metadata.
    pub total_bytes: u64,
    /// Reclaimable free-page bytes, or `None` when the store could not be
    /// sampled without waiting on a live writer.
    pub free_bytes: Option<u64>,
    /// Free pages as a fraction of total pages, or `None` when unsampled.
    pub free_page_ratio: Option<f64>,
}

/// The full report: per-store sizes plus an unregistered-directory backlog
/// summary (plan 38 §2's disjoint on-disk-only audit class, sized here rather
/// than classified — the daemon sweep and `sweep_unregistered_stores` own
/// collection).
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct StorageReport {
    pub profile_root: String,
    pub stores: Vec<StoreSizeReportEntry>,
    pub code_generation_retention: Vec<CodeGenerationRetentionDryRunEntry>,
    #[serde(default)]
    pub code_generation_retention_availability: Vec<CodeGenerationRetentionAvailabilityEntry>,
    pub unregistered_dir_count: usize,
    pub unregistered_bytes: u64,
    pub global_db_bytes: u64,
    /// A direct, read-only census of every regular file under the profile
    /// root. The daemon's bounded per-page response leaves this absent; the
    /// explicit storage-report command attaches it after paging completes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_profile_size: Option<FullProfileSizeV1>,
    pub coverage: StorageReportCoverage,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageReportCoverageState {
    #[default]
    Complete,
    Partial,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct StorageReportCoverage {
    pub state: StorageReportCoverageState,
    pub next_cursor: Option<String>,
}

impl StorageReportCoverage {
    fn partial(next_cursor: String) -> Self {
        Self {
            state: StorageReportCoverageState::Partial,
            next_cursor: Some(next_cursor),
        }
    }
}

/// Whether a profile total accounts for every byte under the profile root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileTotalCoverageStateV1 {
    /// Every byte family under the profile root was sized.
    Complete,
    /// At least one family exists but was not sized; `accounted_bytes` is a
    /// floor, not the profile size.
    Partial,
}

/// Full-profile filesystem census. Symlinks and unreadable entries are never
/// followed or silently treated as zero; they make the total a lower bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct FullProfileSizeV1 {
    pub state: ProfileTotalCoverageStateV1,
    pub total_bytes: u64,
    pub unavailable_entry_count: usize,
}

/// Profile-wide on-disk total, with any family it could not size named.
///
/// A partial total must never read as the profile size. Plan 38 sizes the
/// profile to decide whether retention is keeping up, and a total that quietly
/// omitted a family would understate growth exactly when it matters.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct ProfileTotalSizeV1 {
    pub state: ProfileTotalCoverageStateV1,
    /// Sum of the families below. A floor when `state` is `Partial`.
    pub accounted_bytes: u64,
    /// Graph database families of every registered store in this report.
    pub registered_store_bytes: u64,
    pub global_db_bytes: u64,
    pub unregistered_bytes: u64,
    /// Families known to exist that this report did not fully size.
    pub excluded_families: Vec<String>,
}

impl StorageReport {
    /// Total on-disk bytes this report accounts for, and what it excludes.
    pub fn profile_total_size(&self) -> ProfileTotalSizeV1 {
        let registered_store_bytes = self
            .stores
            .iter()
            .fold(0u64, |total, store| total.saturating_add(store.total_bytes));
        let accounted_bytes = registered_store_bytes
            .saturating_add(self.global_db_bytes)
            .saturating_add(self.unregistered_bytes);

        if let Some(full_profile_size) = self.full_profile_size {
            let excluded_families = (full_profile_size.unavailable_entry_count > 0).then(|| {
                format!(
                    "{} unreadable or non-regular profile entries",
                    full_profile_size.unavailable_entry_count
                )
            });
            return ProfileTotalSizeV1 {
                state: full_profile_size.state,
                accounted_bytes: full_profile_size.total_bytes,
                registered_store_bytes,
                global_db_bytes: self.global_db_bytes,
                unregistered_bytes: self.unregistered_bytes,
                excluded_families: excluded_families.into_iter().collect(),
            };
        }

        let mut excluded_families = Vec::new();
        if self.coverage.state == StorageReportCoverageState::Partial {
            excluded_families.push("registered stores beyond this page".to_owned());
        }
        // Sealed generation files live outside the graph database family, and
        // only their superseded portion is sized here; the active generation is
        // not. Naming the gap keeps the total from posing as the profile size.
        if !self.code_generation_retention.is_empty() {
            excluded_families.push("code-index generation files".to_owned());
        }
        if self
            .code_generation_retention_availability
            .iter()
            .any(|entry| entry.state == StorageReportAvailabilityState::Unavailable)
        {
            excluded_families.push("code-index scopes that could not be read".to_owned());
        }

        let state = if excluded_families.is_empty() {
            ProfileTotalCoverageStateV1::Complete
        } else {
            ProfileTotalCoverageStateV1::Partial
        };
        ProfileTotalSizeV1 {
            state,
            accounted_bytes,
            registered_store_bytes,
            global_db_bytes: self.global_db_bytes,
            unregistered_bytes: self.unregistered_bytes,
            excluded_families,
        }
    }
}

/// Read-only mark-and-sweep preview for one code-index scope.
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct CodeGenerationRetentionDryRunEntry {
    pub project_id: String,
    pub store_root: String,
    pub active_generation_id: String,
    pub active_generation_file: String,
    pub vector_readable_sources: Vec<String>,
    pub rollback_floor: usize,
    pub superseded_generation_count: usize,
    pub superseded_generation_bytes: u64,
    pub collectable_generation_count: usize,
    pub collectable_generation_bytes: u64,
    pub collectable_generations: Vec<CodeGenerationRetentionGenerationV1>,
    /// Whether every listed generation was proven to match the content digest in
    /// its name. False when the digest scan exceeded its budget and the entry
    /// was produced from metadata alone: the counts and byte totals are exact,
    /// the integrity claim is not. Absent in payloads predating this field,
    /// which were only ever emitted after a full verification.
    #[serde(default = "digest_verified_default")]
    pub digest_verified: bool,
}

fn digest_verified_default() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageReportAvailabilityState {
    Available,
    /// The scope was censused from metadata only because verifying every
    /// generation digest exceeded the report's budget. Counts and bytes are
    /// present and exact; this is deliberately not `Unavailable`, which used to
    /// discard a perfectly readable census over a cost the census does not
    /// actually incur.
    MetadataOnly,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct CodeGenerationRetentionAvailabilityEntry {
    pub project_id: String,
    pub store_root: String,
    pub state: StorageReportAvailabilityState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Builds the report by reading `global.db`'s `code_projects` table and every
/// registered project's graph database, then scanning `projects/` bottom-up
/// for directories with no matching registry row. Read-only throughout.
pub async fn build_storage_report(profile_root: &Path) -> crate::errors::Result<StorageReport> {
    let global_db_path = profile_root.join(GLOBAL_DB_FILENAME);
    // Capture the profile's physical footprint before opening any SQLite
    // readers. A report must describe the profile supplied by its caller, not
    // implementation-dependent SQLite sidecar churn caused while collecting
    // the remaining diagnostic fields.
    let full_profile_root = profile_root.to_path_buf();
    let full_profile_size =
        tokio::task::spawn_blocking(move || scan_full_profile_size(&full_profile_root))
            .await
            .map_err(|error| report_error("join full profile size scan", error))?;
    let mut registered_ids = HashSet::new();
    let mut stores = Vec::new();
    let mut code_generation_retention = Vec::new();
    let mut code_generation_retention_availability = Vec::new();

    if global_db_path.exists() {
        let scratch = tempfile::tempdir()
            .map_err(|error| report_error("create external read-snapshot scratch", error))?;
        validate_external_scratch(profile_root, scratch.path())
            .map_err(|error| report_error("validate read-snapshot scratch placement", error))?;
        // Snapshot authority takes a lock beside its source database. Back up
        // the live family to the external scratch first so the authority lock,
        // the immutable reader, and every temporary artifact stay outside the
        // profile being reported. Copy bytes rather than opening the live
        // family: a read-only SQLite open against a WAL database can still
        // materialize a missing SHM sidecar.
        let snapshot_source = scratch.path().join(GLOBAL_DB_FILENAME);
        copy_sqlite_family(&global_db_path, &snapshot_source)
            .map_err(|error| report_error("copy global.db family for read-only report", error))?;
        let snapshot = crate::sqlite_read_snapshot::open_in(&snapshot_source, scratch.path())
            .await
            .map_err(|error| report_error("open global.db read snapshot", error))?;
        let connection = snapshot.connection();
        let mut rows = connection
            .query(
                "SELECT project_id, canonical_root FROM code_projects ORDER BY project_id",
                (),
            )
            .await
            .map_err(|error| report_error("list registered projects", error))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| report_error("read registered project row", error))?
        {
            let project_id: String = row
                .get::<String>(0)
                .map_err(|error| report_error("decode project id", error))?;
            let canonical_root: String = row
                .get::<String>(1)
                .map_err(|error| report_error("decode canonical root", error))?;
            registered_ids.insert(project_id.clone());
            append_project_report(
                profile_root,
                &project_id,
                &canonical_root,
                &mut stores,
                &mut code_generation_retention,
                &mut code_generation_retention_availability,
            )?;
        }
    }

    let (unregistered_dir_count, unregistered_bytes) =
        scan_unregistered_dirs(profile_root, &registered_ids);
    let global_db_bytes = database_family_bytes(&global_db_path);

    Ok(StorageReport {
        profile_root: profile_root.display().to_string(),
        stores,
        code_generation_retention,
        code_generation_retention_availability,
        unregistered_dir_count,
        unregistered_bytes,
        global_db_bytes,
        full_profile_size: Some(full_profile_size),
        coverage: StorageReportCoverage::default(),
    })
}

/// Builds one bounded page through the daemon's retained global registry
/// authority. Registered projects and top-level profile directories are
/// separate cursor phases so neither the registry query nor the filesystem
/// census performs an unbounded profile-wide scan.
pub(crate) async fn build_storage_report_page_from_registered_global_db(
    profile_root: &Path,
    global_db: &crate::global_db::RegisteredGlobalDb,
    cursor: Option<&str>,
    limit: usize,
) -> crate::errors::Result<StorageReport> {
    let limit = limit.clamp(1, MAX_STORAGE_REPORT_PAGE_LIMIT);
    let global_db_bytes = database_family_bytes(&profile_root.join(GLOBAL_DB_FILENAME));
    let cursor = cursor.unwrap_or(PROJECT_CURSOR_PREFIX);
    if let Some(after_project_id) = cursor.strip_prefix(PROJECT_CURSOR_PREFIX) {
        let mut projects = global_db
            .list_code_projects_after(
                (!after_project_id.is_empty()).then_some(after_project_id),
                limit.saturating_add(1),
            )
            .await?;
        let has_more = projects.len() > limit;
        projects.truncate(limit);
        let next_cursor = if has_more {
            let Some(last_project) = projects.last() else {
                return Err(crate::errors::TraceDecayError::Config {
                    message: "project storage report page lost its continuation".to_owned(),
                });
            };
            format!("{PROJECT_CURSOR_PREFIX}{}", last_project.project_id)
        } else {
            DIRECTORY_CURSOR_PREFIX.to_owned()
        };
        let profile_root = profile_root.to_path_buf();
        return tokio::task::spawn_blocking(move || {
            let mut report = StorageReport {
                profile_root: profile_root.display().to_string(),
                global_db_bytes,
                coverage: StorageReportCoverage::partial(next_cursor),
                ..StorageReport::default()
            };
            for project in projects {
                append_project_report(
                    &profile_root,
                    &project.project_id,
                    &project.canonical_root,
                    &mut report.stores,
                    &mut report.code_generation_retention,
                    &mut report.code_generation_retention_availability,
                )?;
            }
            Ok(report)
        })
        .await
        .map_err(|error| report_error("join daemon-backed storage report page", error))?;
    }
    let Some(after_directory) = cursor.strip_prefix(DIRECTORY_CURSOR_PREFIX) else {
        return Err(crate::errors::TraceDecayError::Config {
            message: "invalid daemon storage report cursor".to_owned(),
        });
    };
    let profile_root_buf = profile_root.to_path_buf();
    let after_directory = after_directory.to_owned();
    let (directories, has_more) = tokio::task::spawn_blocking(move || {
        list_project_directories_page(&profile_root_buf, &after_directory, limit)
    })
    .await
    .map_err(|error| report_error("join storage directory page", error))?;
    let mut unregistered = Vec::new();
    for (name, path) in &directories {
        if !global_db.code_project_exists(name).await? {
            unregistered.push(path.clone());
        }
    }
    let (unregistered_dir_count, unregistered_bytes) = tokio::task::spawn_blocking(move || {
        let bytes = unregistered.iter().fold(0u64, |total, path| {
            total.saturating_add(super::orphan_stores::dir_size_bytes(path))
        });
        (unregistered.len(), bytes)
    })
    .await
    .map_err(|error| report_error("join unregistered storage page", error))?;
    let next_cursor = has_more
        .then(|| {
            directories
                .last()
                .map(|(name, _)| format!("{DIRECTORY_CURSOR_PREFIX}{name}"))
        })
        .flatten();
    Ok(StorageReport {
        profile_root: profile_root.display().to_string(),
        unregistered_dir_count,
        unregistered_bytes,
        global_db_bytes,
        coverage: next_cursor.map_or_else(
            StorageReportCoverage::default,
            StorageReportCoverage::partial,
        ),
        ..StorageReport::default()
    })
}

fn list_project_directories_page(
    profile_root: &Path,
    after_directory: &str,
    limit: usize,
) -> (Vec<(String, std::path::PathBuf)>, bool) {
    let Ok(entries) = std::fs::read_dir(profile_root.join("projects")) else {
        return (Vec::new(), false);
    };
    let mut directories = entries
        .flatten()
        .filter_map(|entry| {
            entry.file_type().ok().filter(std::fs::FileType::is_dir)?;
            let name = entry.file_name().into_string().ok()?;
            (name.as_str() > after_directory).then(|| (name, entry.path()))
        })
        .collect::<Vec<_>>();
    directories.sort_by(|left, right| left.0.cmp(&right.0));
    let has_more = directories.len() > limit;
    directories.truncate(limit);
    (directories, has_more)
}

/// Builds one project-scoped report on the daemon's blocking pool so bounded
/// read-only `SQLite` samples cannot stall the async authority loop.
pub async fn build_project_storage_report_from_daemon(
    profile_root: &Path,
    project_id: &str,
    canonical_root: &Path,
) -> crate::errors::Result<StorageReport> {
    let profile_root = profile_root.to_path_buf();
    let project_id = project_id.to_owned();
    let canonical_root = canonical_root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        build_project_storage_report(&profile_root, &project_id, &canonical_root)
    })
    .await
    .map_err(|error| report_error("join daemon-backed project storage report", error))?
}

/// Build the same read-only report for one explicitly identified shard without
/// opening `global.db`. This is the daemon-independent path for maintenance
/// when the global registry's exclusive-maintenance authority is unavailable.
pub fn build_project_storage_report(
    profile_root: &Path,
    project_id: &str,
    canonical_root: &Path,
) -> crate::errors::Result<StorageReport> {
    crate::storage::validate_project_id(project_id).map_err(|message| {
        crate::errors::TraceDecayError::Config {
            message: message.to_owned(),
        }
    })?;
    let mut stores = Vec::new();
    let mut code_generation_retention = Vec::new();
    let mut code_generation_retention_availability = Vec::new();
    append_project_report(
        profile_root,
        project_id,
        &canonical_root.to_string_lossy(),
        &mut stores,
        &mut code_generation_retention,
        &mut code_generation_retention_availability,
    )?;
    let global_db_path = profile_root.join(GLOBAL_DB_FILENAME);
    Ok(StorageReport {
        profile_root: profile_root.display().to_string(),
        stores,
        code_generation_retention,
        code_generation_retention_availability,
        unregistered_dir_count: 0,
        unregistered_bytes: 0,
        global_db_bytes: database_family_bytes(&global_db_path),
        full_profile_size: None,
        coverage: StorageReportCoverage::default(),
    })
}

fn append_project_report(
    profile_root: &Path,
    project_id: &str,
    canonical_root: &str,
    stores: &mut Vec<StoreSizeReportEntry>,
    code_generation_retention: &mut Vec<CodeGenerationRetentionDryRunEntry>,
    code_generation_retention_availability: &mut Vec<CodeGenerationRetentionAvailabilityEntry>,
) -> crate::errors::Result<()> {
    let data_root = profile_root.join("projects").join(project_id);
    let graph_db_path = data_root.join(crate::config::DB_FILENAME);
    if let Some(entry) = sample_store_size(&graph_db_path) {
        stores.push(StoreSizeReportEntry {
            project_id: project_id.to_owned(),
            canonical_root: canonical_root.to_owned(),
            total_bytes: entry.total_bytes,
            free_bytes: entry.free_bytes,
            free_page_ratio: entry.free_page_ratio,
        });
    }
    let code_index_store_root =
        scoped_code_index_store_root(&data_root.join("code-index-v1"), Path::new(canonical_root));
    if !code_index_store_root
        .join("active-code-generation-v1.json")
        .is_file()
    {
        return Ok(());
    }
    let digest_scan_exceeds_budget = match generation_digest_scan_exceeds_budget(
        &code_index_store_root,
        CODE_GENERATION_RETENTION_DIGEST_SCAN_MAX_BYTES,
    ) {
        Ok(exceeds_budget) => exceeds_budget,
        Err(_) => {
            code_generation_retention_availability.push(CodeGenerationRetentionAvailabilityEntry {
                project_id: project_id.to_owned(),
                store_root: code_index_store_root.display().to_string(),
                state: StorageReportAvailabilityState::Unavailable,
                reason: Some("generation_retention_scan_unavailable".to_owned()),
            });
            return Ok(());
        }
    };
    // Exceeding the digest budget bounds how hard the census may *verify*, not
    // whether it may run. A single sealed generation is routinely larger than
    // any budget cheap enough to be worth having, so treating this as
    // "unavailable" reported nothing at all on exactly the profiles that had
    // something to report.
    let verification = if digest_scan_exceeds_budget {
        GenerationDigestVerificationV1::MetadataOnly
    } else {
        GenerationDigestVerificationV1::Full
    };
    let readable_sources = match crate::store::vector_generations::retained_readable_sources_from_read_only_project_store(
            &data_root,
        ) {
        Ok(readable_sources) => readable_sources,
        Err(_) => {
            code_generation_retention_availability.push(CodeGenerationRetentionAvailabilityEntry {
                project_id: project_id.to_owned(),
                store_root: code_index_store_root.display().to_string(),
                state: StorageReportAvailabilityState::Unavailable,
                reason: Some("generation_retention_liveness_unavailable".to_owned()),
            });
            return Ok(());
        }
    };
    let plan = match plan_code_generation_retention_with_verification(
        &code_index_store_root,
        &readable_sources,
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        verification,
    ) {
        Ok(plan) => plan,
        Err(_) => {
            code_generation_retention_availability.push(CodeGenerationRetentionAvailabilityEntry {
                project_id: project_id.to_owned(),
                store_root: code_index_store_root.display().to_string(),
                state: StorageReportAvailabilityState::Unavailable,
                reason: Some("generation_retention_plan_unavailable".to_owned()),
            });
            return Ok(());
        }
    };
    code_generation_retention.push(CodeGenerationRetentionDryRunEntry {
        project_id: project_id.to_owned(),
        store_root: code_index_store_root.display().to_string(),
        active_generation_id: plan.active_generation_id.as_str().to_owned(),
        active_generation_file: plan.active_generation_file().to_owned(),
        vector_readable_sources: plan
            .vector_readable_sources
            .iter()
            .map(|source| source.as_str().to_owned())
            .collect(),
        rollback_floor: plan.rollback_floor,
        superseded_generation_count: plan.superseded_generations.len(),
        superseded_generation_bytes: plan.superseded_generation_bytes(),
        collectable_generation_count: plan.collectable_generations.len(),
        collectable_generation_bytes: plan.collectable_generation_bytes(),
        collectable_generations: plan.collectable_generations,
        digest_verified: verification == GenerationDigestVerificationV1::Full,
    });
    let (state, reason) = match verification {
        GenerationDigestVerificationV1::Full => (StorageReportAvailabilityState::Available, None),
        GenerationDigestVerificationV1::MetadataOnly => (
            StorageReportAvailabilityState::MetadataOnly,
            Some("generation_digest_scan_budget_exceeded".to_owned()),
        ),
    };
    code_generation_retention_availability.push(CodeGenerationRetentionAvailabilityEntry {
        project_id: project_id.to_owned(),
        store_root: code_index_store_root.display().to_string(),
        state,
        reason,
    });
    Ok(())
}

fn generation_digest_scan_exceeds_budget(
    store_root: &Path,
    maximum_bytes: u64,
) -> crate::errors::Result<bool> {
    let entries = std::fs::read_dir(store_root.join(CODE_GENERATIONS_DIRECTORY))
        .map_err(|error| report_error("list code-generation retention files", error))?;
    let mut bytes = 0u64;
    for entry in entries {
        let entry =
            entry.map_err(|error| report_error("read code-generation retention entry", error))?;
        if !entry
            .file_type()
            .map_err(|error| report_error("read code-generation retention file type", error))?
            .is_file()
        {
            continue;
        }
        bytes = bytes.saturating_add(
            entry
                .metadata()
                .map_err(|error| report_error("size code-generation retention file", error))?
                .len(),
        );
        if bytes > maximum_bytes {
            return Ok(true);
        }
    }
    Ok(false)
}

/// A store's sampled size. `total_bytes` is always available (filesystem
/// metadata); the free-page fields are only present when the database could
/// be read without waiting on a live writer.
struct StoreSizeSample {
    total_bytes: u64,
    free_bytes: Option<u64>,
    free_page_ratio: Option<f64>,
}

/// `None` only when the store has no graph database file at all.
fn sample_store_size(graph_db_path: &Path) -> Option<StoreSizeSample> {
    if !graph_db_path.is_file() {
        return None;
    }
    // Filesystem metadata over the whole family: what the owner actually sees
    // consumed on disk, including a WAL that has not been checkpointed.
    let total_bytes = database_family_bytes(graph_db_path);
    let (free_bytes, free_page_ratio) = match sample_free_pages(graph_db_path) {
        Some((free_bytes, ratio)) => (Some(free_bytes), Some(ratio)),
        None => (None, None),
    };
    Some(StoreSizeSample {
        total_bytes,
        free_bytes,
        free_page_ratio,
    })
}

fn database_family_bytes(database_path: &Path) -> u64 {
    let mut total_bytes = 0u64;
    for suffix in ["", "-wal", "-shm"] {
        if let Ok(metadata) = std::fs::metadata(sqlite_family_member(database_path, suffix)) {
            total_bytes = total_bytes.saturating_add(metadata.len());
        }
    }
    total_bytes
}

/// Copies a `SQLite` main/WAL/SHM family without opening the source path. The
/// caller opens only `destination`, which prevents reporting from creating
/// sidecars under the profile it is inspecting.
fn copy_sqlite_family(source: &Path, destination: &Path) -> std::io::Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let source_member = sqlite_family_member(source, suffix);
        if !source_member.is_file() {
            continue;
        }
        std::fs::copy(&source_member, sqlite_family_member(destination, suffix))?;
    }
    Ok(())
}

fn validate_external_scratch(profile_root: &Path, scratch_root: &Path) -> std::io::Result<()> {
    let profile = profile_root.canonicalize()?;
    let scratch = scratch_root.canonicalize()?;
    if scratch.starts_with(profile) {
        return Err(std::io::Error::other(
            "read-snapshot scratch must be outside the inspected profile",
        ));
    }
    Ok(())
}

fn sqlite_family_member(database_path: &Path, suffix: &str) -> PathBuf {
    if suffix.is_empty() {
        database_path.to_path_buf()
    } else {
        let mut name = database_path.as_os_str().to_os_string();
        name.push(suffix);
        PathBuf::from(name)
    }
}

/// Reads the free-page pragmas over the shared bounded read-only probe.
/// Returns `None` on any failure — a store held by a busy writer, a corrupt
/// file, or a WAL database whose `-shm` cannot be mapped read-only. A report
/// must degrade, never block and never repair.
fn sample_free_pages(graph_db_path: &Path) -> Option<(u64, f64)> {
    let connection = open_read_only_probe(graph_db_path, BOUNDED_PROBE_BUSY_TIMEOUT).ok()?;
    let page_size = pragma_u64(&connection, "page_size")?;
    let page_count = pragma_u64(&connection, "page_count")?;
    let freelist = pragma_u64(&connection, "freelist_count")?;
    if page_size == 0 || page_count == 0 {
        return None;
    }
    let free_bytes = page_size.saturating_mul(freelist);
    #[allow(clippy::cast_precision_loss)]
    let free_page_ratio = freelist as f64 / page_count as f64;
    Some((free_bytes, free_page_ratio))
}

fn scan_unregistered_dirs(profile_root: &Path, registered_ids: &HashSet<String>) -> (usize, u64) {
    let projects_dir = profile_root.join("projects");
    let Ok(entries) = std::fs::read_dir(&projects_dir) else {
        return (0, 0);
    };
    let mut count = 0usize;
    let mut bytes = 0u64;
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if crate::storage::validate_project_id(&name).is_err() || registered_ids.contains(&name) {
            continue;
        }
        count += 1;
        // Shares the orphan sweep's walker so the bytes this report shows for
        // a backlog directory are the same bytes that sweep would reclaim.
        bytes = bytes.saturating_add(super::orphan_stores::dir_size_bytes(&entry.path()));
    }
    (count, bytes)
}

/// Count every regular file under a profile root without following symlinks.
/// Failures remain visible as a partial lower bound instead of a successful
/// zero-size family.
pub(crate) fn scan_full_profile_size(profile_root: &Path) -> FullProfileSizeV1 {
    fn walk(path: &Path, total_bytes: &mut u64, unavailable_entry_count: &mut usize) {
        let entries = match std::fs::read_dir(path) {
            Ok(entries) => entries,
            Err(_) => {
                *unavailable_entry_count = unavailable_entry_count.saturating_add(1);
                return;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    *unavailable_entry_count = unavailable_entry_count.saturating_add(1);
                    continue;
                }
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => {
                    *unavailable_entry_count = unavailable_entry_count.saturating_add(1);
                    continue;
                }
            };
            if file_type.is_dir() {
                walk(&entry.path(), total_bytes, unavailable_entry_count);
            } else if file_type.is_file() {
                match entry.metadata() {
                    Ok(metadata) => {
                        *total_bytes = total_bytes.saturating_add(metadata.len());
                    }
                    Err(_) => {
                        *unavailable_entry_count = unavailable_entry_count.saturating_add(1);
                    }
                }
            } else {
                *unavailable_entry_count = unavailable_entry_count.saturating_add(1);
            }
        }
    }

    let mut total_bytes = 0u64;
    let mut unavailable_entry_count = 0usize;
    walk(profile_root, &mut total_bytes, &mut unavailable_entry_count);
    FullProfileSizeV1 {
        state: if unavailable_entry_count == 0 {
            ProfileTotalCoverageStateV1::Complete
        } else {
            ProfileTotalCoverageStateV1::Partial
        },
        total_bytes,
        unavailable_entry_count,
    }
}

fn report_error(
    operation: &'static str,
    error: impl std::fmt::Display,
) -> crate::errors::TraceDecayError {
    crate::errors::TraceDecayError::Database {
        operation: operation.to_string(),
        message: error.to_string(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::db::engine::TestConnection;

    fn store(project_id: &str, total_bytes: u64) -> StoreSizeReportEntry {
        StoreSizeReportEntry {
            project_id: project_id.to_owned(),
            canonical_root: format!("/work/{project_id}"),
            total_bytes,
            free_bytes: None,
            free_page_ratio: None,
        }
    }

    #[test]
    fn profile_total_sums_every_measured_family() {
        let report = StorageReport {
            stores: vec![store("alpha", 400), store("beta", 600)],
            global_db_bytes: 100,
            unregistered_bytes: 25,
            ..StorageReport::default()
        };

        let total = report.profile_total_size();
        assert_eq!(total.state, ProfileTotalCoverageStateV1::Complete);
        assert_eq!(total.registered_store_bytes, 1_000);
        assert_eq!(total.accounted_bytes, 1_125);
        assert!(total.excluded_families.is_empty());
    }

    #[test]
    fn paginated_report_totals_a_floor_and_never_claims_completeness() {
        let report = StorageReport {
            stores: vec![store("alpha", 400)],
            global_db_bytes: 100,
            coverage: StorageReportCoverage::partial("alpha".to_owned()),
            ..StorageReport::default()
        };

        let total = report.profile_total_size();
        assert_eq!(
            total.state,
            ProfileTotalCoverageStateV1::Partial,
            "a page of stores cannot total the whole profile"
        );
        assert_eq!(total.accounted_bytes, 500);
        assert!(
            total
                .excluded_families
                .iter()
                .any(|family| family.contains("beyond this page"))
        );
    }

    #[test]
    fn unreadable_code_index_scope_is_named_rather_than_silently_dropped() {
        let report = StorageReport {
            stores: vec![store("alpha", 400)],
            code_generation_retention_availability: vec![
                CodeGenerationRetentionAvailabilityEntry {
                    project_id: "alpha".to_owned(),
                    store_root: "/profile/projects/alpha/code-index-v1/ab".to_owned(),
                    state: StorageReportAvailabilityState::Unavailable,
                    reason: Some("generation_digest_scan_budget_exceeded".to_owned()),
                },
            ],
            ..StorageReport::default()
        };

        let total = report.profile_total_size();
        assert_eq!(total.state, ProfileTotalCoverageStateV1::Partial);
        assert!(
            total
                .excluded_families
                .iter()
                .any(|family| family.contains("could not be read"))
        );
    }

    #[test]
    fn empty_profile_totals_zero_only_when_nothing_is_excluded() {
        let total = StorageReport::default().profile_total_size();
        assert_eq!(total.accounted_bytes, 0);
        assert_eq!(
            total.state,
            ProfileTotalCoverageStateV1::Complete,
            "a genuinely empty profile is a complete zero, not a partial one"
        );
    }

    #[test]
    fn store_byte_overflow_saturates_instead_of_wrapping() {
        let report = StorageReport {
            stores: vec![store("alpha", u64::MAX), store("beta", 10)],
            global_db_bytes: 10,
            ..StorageReport::default()
        };

        let total = report.profile_total_size();
        assert_eq!(total.registered_store_bytes, u64::MAX);
        assert_eq!(total.accounted_bytes, u64::MAX);
    }

    async fn seed_global_db(profile_root: &Path, projects: &[(&str, &str)]) {
        let conn = TestConnection::open(&profile_root.join(GLOBAL_DB_FILENAME));
        conn.execute_batch(
            "CREATE TABLE code_projects (
                project_id TEXT PRIMARY KEY,
                canonical_root TEXT NOT NULL
             );",
        )
        .await
        .unwrap();
        for (project_id, canonical_root) in projects {
            conn.execute(
                "INSERT INTO code_projects (project_id, canonical_root) VALUES (?1, ?2)",
                crate::db::engine::params![*project_id, *canonical_root],
            )
            .await
            .unwrap();
        }
    }

    fn seed_graph_db(profile_root: &Path, project_id: &str) {
        let data_root = profile_root.join("projects").join(project_id);
        std::fs::create_dir_all(&data_root).unwrap();
        let connection =
            rusqlite::Connection::open(data_root.join(crate::config::DB_FILENAME)).unwrap();
        connection
            .execute_batch("CREATE TABLE fixture (id INTEGER PRIMARY KEY);")
            .unwrap();
    }

    fn profile_tree_bytes(root: &Path) -> u64 {
        std::fs::read_dir(root)
            .unwrap()
            .flatten()
            .fold(0u64, |total, entry| {
                let file_type = entry.file_type().unwrap();
                if file_type.is_dir() {
                    total.saturating_add(profile_tree_bytes(&entry.path()))
                } else if file_type.is_file() {
                    total.saturating_add(entry.metadata().unwrap().len())
                } else {
                    total
                }
            })
    }

    fn profile_entries(root: &Path) -> BTreeSet<PathBuf> {
        fn collect(root: &Path, current: &Path, entries: &mut BTreeSet<PathBuf>) {
            for entry in std::fs::read_dir(current).unwrap().flatten() {
                let path = entry.path();
                entries.insert(path.strip_prefix(root).unwrap().to_path_buf());
                if entry.file_type().unwrap().is_dir() {
                    collect(root, &path, entries);
                }
            }
        }

        let mut entries = BTreeSet::new();
        collect(root, root, &mut entries);
        entries
    }

    fn sqlite_family_bytes(path: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        ["", "-wal", "-shm"]
            .into_iter()
            .filter_map(|suffix| {
                let member = if suffix.is_empty() {
                    path.to_path_buf()
                } else {
                    let mut name = path.as_os_str().to_os_string();
                    name.push(suffix);
                    PathBuf::from(name)
                };
                member
                    .is_file()
                    .then(|| (member.clone(), std::fs::read(member).unwrap()))
            })
            .collect()
    }

    #[tokio::test]
    async fn report_sizes_every_registered_store_and_counts_unregistered_dirs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_root = tmp.path().join("profile");
        std::fs::create_dir_all(&profile_root).unwrap();
        seed_global_db(&profile_root, &[("proj_a", "/repos/a")]).await;
        seed_graph_db(&profile_root, "proj_a");

        // An unregistered leaf directory under `projects/`.
        let ghost = profile_root.join("projects").join("proj_ghost");
        std::fs::create_dir_all(&ghost).unwrap();
        std::fs::write(ghost.join("payload.bin"), vec![0u8; 2048]).unwrap();

        let report = build_storage_report(&profile_root).await.unwrap();

        assert_eq!(report.stores.len(), 1);
        assert_eq!(report.stores[0].project_id, "proj_a");
        assert_eq!(report.stores[0].canonical_root, "/repos/a");
        assert!(report.stores[0].total_bytes > 0);
        assert_eq!(report.unregistered_dir_count, 1);
        assert!(report.unregistered_bytes >= 2048);
    }

    #[tokio::test]
    async fn full_profile_total_includes_session_and_generation_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_root = tmp.path().join("profile");
        std::fs::create_dir_all(&profile_root).unwrap();
        seed_global_db(&profile_root, &[("proj_a", "/repos/a")]).await;
        seed_graph_db(&profile_root, "proj_a");

        std::fs::write(profile_root.join("sessions.db"), vec![1u8; 2_048]).unwrap();
        let generations = profile_root
            .join("projects")
            .join("proj_a")
            .join("code-index-v1")
            .join("fixture")
            .join(CODE_GENERATIONS_DIRECTORY);
        std::fs::create_dir_all(&generations).unwrap();
        std::fs::write(generations.join("generation-active.json"), vec![2u8; 4_096]).unwrap();
        std::fs::write(generations.join("generation-pinned.json"), vec![3u8; 1_024]).unwrap();

        let expected_bytes = profile_tree_bytes(&profile_root);
        let report = build_storage_report(&profile_root).await.unwrap();
        let total = report.profile_total_size();

        assert_eq!(total.state, ProfileTotalCoverageStateV1::Complete);
        assert_eq!(
            total.accounted_bytes, expected_bytes,
            "a full profile total must include session and code-generation files, \
             not only registered graph databases"
        );
    }

    #[tokio::test]
    async fn full_profile_report_creates_no_entries_under_the_profile_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_root = tmp.path().join("profile");
        std::fs::create_dir_all(&profile_root).unwrap();
        seed_global_db(&profile_root, &[("proj_a", "/repos/a")]).await;
        seed_graph_db(&profile_root, "proj_a");
        let before = profile_entries(&profile_root);

        let report = build_storage_report(&profile_root).await.unwrap();

        assert_eq!(
            report.profile_total_size().state,
            ProfileTotalCoverageStateV1::Complete
        );
        assert_eq!(
            profile_entries(&profile_root),
            before,
            "read-only reporting must not add scratch or other entries to a live profile"
        );
    }

    #[tokio::test]
    async fn storage_report_preserves_the_exact_live_global_database_family() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_root = tmp.path().join("profile");
        std::fs::create_dir_all(&profile_root).unwrap();
        seed_global_db(&profile_root, &[("proj_a", "/repos/a")]).await;
        let global_db = profile_root.join(GLOBAL_DB_FILENAME);
        let before = sqlite_family_bytes(&global_db);

        build_storage_report(&profile_root).await.unwrap();

        assert_eq!(
            sqlite_family_bytes(&global_db),
            before,
            "reporting must never create, change, or delete a source SQLite family member"
        );
    }

    #[test]
    fn scratch_validation_rejects_a_transient_directory_inside_the_profile() {
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("profile");
        let transient = profile.join("transient");
        std::fs::create_dir_all(&transient).unwrap();

        assert!(validate_external_scratch(&profile, &transient).is_err());
    }

    /// The report must never freeze a store family to size it: a snapshot
    /// reflinks or, when the filesystem cannot, fully copies the database.
    /// Sizing must come from metadata and leave no scratch behind.
    #[tokio::test]
    async fn sizing_a_store_copies_nothing_and_leaves_no_scratch() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_root = tmp.path().join("profile");
        std::fs::create_dir_all(&profile_root).unwrap();
        seed_global_db(&profile_root, &[("proj_a", "/repos/a")]).await;
        seed_graph_db(&profile_root, "proj_a");

        let graph_db = profile_root
            .join("projects")
            .join("proj_a")
            .join(crate::config::DB_FILENAME);
        let before = std::fs::metadata(&graph_db).unwrap().modified().unwrap();

        let report = build_storage_report(&profile_root).await.unwrap();

        assert_eq!(report.stores.len(), 1);
        assert_eq!(
            report.stores[0].total_bytes,
            std::fs::metadata(&graph_db).unwrap().len(),
            "size must come from filesystem metadata"
        );
        assert!(
            report.stores[0].free_page_ratio.is_some(),
            "an idle store must sample its free pages"
        );
        assert_eq!(
            std::fs::metadata(&graph_db).unwrap().modified().unwrap(),
            before,
            "sizing must not touch the source database"
        );
        assert!(
            !profile_root
                .join("projects")
                .join("proj_a")
                .join("scratch")
                .exists(),
            "sizing must not write scratch into the store"
        );
    }

    /// A registered store whose graph database is unreadable still reports its
    /// on-disk size; only the free-page fields degrade to `None`. Reporting a
    /// zero ratio there would read as "no bloat".
    #[tokio::test]
    async fn an_unreadable_store_reports_size_with_unsampled_free_pages() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_root = tmp.path().join("profile");
        std::fs::create_dir_all(&profile_root).unwrap();
        seed_global_db(&profile_root, &[("proj_a", "/repos/a")]).await;

        let data_root = profile_root.join("projects").join("proj_a");
        std::fs::create_dir_all(&data_root).unwrap();
        // Not a SQLite database at all.
        std::fs::write(data_root.join(crate::config::DB_FILENAME), vec![9u8; 4096]).unwrap();

        let report = build_storage_report(&profile_root).await.unwrap();

        assert_eq!(report.stores.len(), 1);
        assert_eq!(report.stores[0].total_bytes, 4096);
        assert_eq!(report.stores[0].free_bytes, None);
        assert_eq!(report.stores[0].free_page_ratio, None);
    }

    /// The backlog walker must not follow symlinks: one pointing at an
    /// ancestor would recurse until the stack ran out, and one pointing
    /// outside would bill another tree's bytes to the backlog.
    #[cfg(unix)]
    #[tokio::test]
    async fn unregistered_sizing_does_not_follow_symlinks() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_root = tmp.path().join("profile");
        std::fs::create_dir_all(&profile_root).unwrap();
        seed_global_db(&profile_root, &[]).await;

        let ghost = profile_root.join("projects").join("proj_ghost");
        std::fs::create_dir_all(&ghost).unwrap();
        std::fs::write(ghost.join("payload.bin"), vec![0u8; 2048]).unwrap();
        // A loop back to the directory being walked.
        std::os::unix::fs::symlink(&ghost, ghost.join("loop")).unwrap();
        // And an escape hatch pointing at a large tree outside the profile.
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("big.bin"), vec![0u8; 65536]).unwrap();
        std::os::unix::fs::symlink(&outside, ghost.join("escape")).unwrap();

        let report = build_storage_report(&profile_root).await.unwrap();

        assert_eq!(report.unregistered_dir_count, 1);
        assert_eq!(
            report.unregistered_bytes, 2048,
            "only the store's own bytes count"
        );
    }

    #[tokio::test]
    async fn report_on_empty_profile_root_is_empty_not_an_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_root = tmp.path().join("profile");
        std::fs::create_dir_all(&profile_root).unwrap();

        let report = build_storage_report(&profile_root).await.unwrap();

        assert!(report.stores.is_empty());
        assert_eq!(report.unregistered_dir_count, 0);
        assert_eq!(report.global_db_bytes, 0);
    }

    #[test]
    fn targeted_project_report_bypasses_global_registry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_root = tmp.path().join("profile");
        std::fs::create_dir_all(&profile_root).unwrap();
        seed_graph_db(&profile_root, "proj_a");

        let report =
            build_project_storage_report(&profile_root, "proj_a", Path::new("/repos/a")).unwrap();

        assert_eq!(report.stores.len(), 1);
        assert_eq!(report.stores[0].project_id, "proj_a");
        assert_eq!(report.stores[0].canonical_root, "/repos/a");
        assert!(report.code_generation_retention.is_empty());
        assert!(!profile_root.join(GLOBAL_DB_FILENAME).exists());
    }

    #[test]
    fn unreadable_generation_retention_is_typed_unavailable() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_root = tmp.path().join("profile");
        std::fs::create_dir_all(&profile_root).unwrap();
        seed_graph_db(&profile_root, "proj_a");
        let canonical_root = Path::new("/repos/a");
        let data_root = profile_root.join("projects").join("proj_a");
        let store_root =
            scoped_code_index_store_root(&data_root.join("code-index-v1"), canonical_root);
        std::fs::create_dir_all(store_root.join(CODE_GENERATIONS_DIRECTORY)).unwrap();
        std::fs::write(store_root.join("active-code-generation-v1.json"), b"{}").unwrap();

        let report = build_project_storage_report(&profile_root, "proj_a", canonical_root).unwrap();

        assert!(report.code_generation_retention.is_empty());
        let availability = report
            .code_generation_retention_availability
            .first()
            .expect("unavailable retention state");
        assert_eq!(
            availability.project_id, "proj_a",
            "the unavailable state must name its owning project"
        );
        assert_eq!(availability.store_root, store_root.display().to_string());
        assert_eq!(
            availability.state,
            StorageReportAvailabilityState::Unavailable
        );
        assert!(
            availability
                .reason
                .as_deref()
                .is_some_and(|reason| reason.ends_with("_unavailable"))
        );
    }

    /// Exceeding the digest budget must not by itself discard the census. The
    /// fixture's active pointer is deliberately corrupt, so the metadata-only
    /// fallback still refuses — but it refuses for the *pointer*, proving the
    /// budget no longer short-circuits ahead of it.
    #[test]
    fn oversized_generation_digest_scan_falls_through_to_the_metadata_census() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_root = tmp.path().join("profile");
        std::fs::create_dir_all(&profile_root).unwrap();
        seed_graph_db(&profile_root, "proj_a");
        let canonical_root = Path::new("/repos/a");
        let data_root = profile_root.join("projects").join("proj_a");
        let store_root =
            scoped_code_index_store_root(&data_root.join("code-index-v1"), canonical_root);
        let generations = store_root.join("code-generations-v1");
        std::fs::create_dir_all(&generations).unwrap();
        std::fs::write(store_root.join("active-code-generation-v1.json"), b"{}").unwrap();
        let oversized = std::fs::File::create(generations.join("generation-oversized.json"))
            .expect("oversized generation fixture");
        oversized
            .set_len(64 * 1024 * 1024)
            .expect("sparse oversized generation");

        let report = build_project_storage_report(&profile_root, "proj_a", canonical_root).unwrap();
        let payload = serde_json::to_value(report).unwrap();

        assert_ne!(
            payload["code_generation_retention_availability"][0]["reason"],
            "generation_digest_scan_budget_exceeded",
            "the digest budget must bound verification, not availability: {payload}"
        );
        assert_eq!(
            payload["code_generation_retention_availability"][0]["state"], "unavailable",
            "a corrupt active pointer is still unavailable: {payload}"
        );
    }

    #[test]
    fn metadata_only_state_is_not_treated_as_an_unreadable_scope() {
        let report = StorageReport {
            stores: vec![store("alpha", 400)],
            code_generation_retention_availability: vec![
                CodeGenerationRetentionAvailabilityEntry {
                    project_id: "alpha".to_owned(),
                    store_root: "/profile/projects/alpha/code-index-v1/ab".to_owned(),
                    state: StorageReportAvailabilityState::MetadataOnly,
                    reason: Some("generation_digest_scan_budget_exceeded".to_owned()),
                },
            ],
            ..StorageReport::default()
        };

        let total = report.profile_total_size();

        assert!(
            !total
                .excluded_families
                .iter()
                .any(|family| family.contains("could not be read")),
            "a metadata-only census read the scope; it just did not re-hash it"
        );
    }
}
