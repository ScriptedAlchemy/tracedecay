//! Read-only, cheap-to-query storage observability (plan 38 §7): per-store
//! size and free-page ratio for every registered profile-sharded store under
//! a profile root, plus an unregistered-directory backlog summary reachable
//! from `tracedecay storage report` without a live daemon or any
//! [`tracedecay_global_db::RegisteredGlobalDb`] writer authority.
//!
//! # Why this does not snapshot stores in place
//!
//! The registry read does, because it is one small file and needs a
//! consistent view of `code_projects`. The *per-store size sampling*
//! deliberately does not. [`tracedecay_runtime_core::sqlite_read_snapshot`] freezes a database
//! family by reflinking it, falling back to a **full byte copy** when the
//! filesystem cannot reflink, and any graph database a live daemon has open
//! is WAL-backed and therefore takes that path. Running this report over the
//! profile it was written for would have copied every registered store to
//! read three pragmas off it. The owner's profile was 91GB, on a command
//! whose entire promise is being cheap enough to run on a full profile.
//!
//! So sizes come from filesystem metadata (exact, no locks, no I/O beyond
//! `stat`) and free-page counts from a short read-only connection. The one
//! registry snapshot uses an OS-temporary scratch directory, never a child of
//! the profile it inspects. If a free-page connection fails because the store
//! is busy, corrupt, or a WAL database with no `-shm` to map read-only, its
//! fields are `None` rather than guessed at, and the store still reports its
//! size.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use tracedecay_domain::errors::TraceDecayError;
use tracedecay_domain::{
    CodeGenerationId, CursorBindingMismatchV1, CursorBindingV1, decode_bound_cursor,
    encode_bound_cursor,
};
use tracedecay_runtime_core::sqlite_read_snapshot::{
    BOUNDED_PROBE_BUSY_TIMEOUT, open_read_only_probe, pragma_u64,
};

use tracedecay_code_index_retention::code_index_generations::{
    CODE_TEXT_ARTIFACT_STAGING_DIRECTORY_V1, CODE_TEXT_ARTIFACTS_DIRECTORY_V1,
    CodeGenerationRetentionGenerationV1, GenerationDigestVerificationV1,
    plan_code_generation_retention_with_verification, scoped_code_index_store_root,
};
use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1;
use tracedecay_runtime_core::storage::SESSIONS_DB_FILENAME;

use crate::store_maintenance::{CodeGenerationProtectionUnavailableV1, code_generation_protection};

const GLOBAL_DB_FILENAME: &str = "global.db";
pub const MAX_STORAGE_REPORT_PAGE_LIMIT: usize = 64;
const CODE_GENERATION_RETENTION_DIGEST_SCAN_MAX_BYTES: u64 = 32 * 1024 * 1024;
const CODE_GENERATIONS_DIRECTORY: &str = "code-generations-v1";
const CODE_INDEX_DIRECTORY: &str = "code-index-v1";
const SEALED_GRAPH_DIRECTORY: &str = "tracedecay.sealed";
const GRAPH_REPLAY_POOL_DIRECTORY: &str = "tracedecay.graph-replay";
/// Why a report without the daemon's scheduler authority cannot plan a
/// retention dry run.
pub const RETENTION_PROTECTION_UNRESOLVED: &str =
    "generation_retention_graph_inventory_unavailable";

/// The protection set a retention dry run plans against, or why it could not
/// be resolved.
type RetentionProtection = Result<BTreeSet<CodeGenerationId>, &'static str>;

/// One registered profile-sharded store's size/free-page snapshot.
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct StoreSizeReportEntry {
    pub project_id: String,
    pub canonical_root: String,
    /// Every regular file under the store's `projects/<id>/` directory, from
    /// filesystem metadata: the sum of `kinds`.
    pub total_bytes: u64,
    /// `total_bytes` by the store family each file belongs to.
    pub kinds: StoreKindBytesV1,
    /// Entries under the store that could not be read or are neither regular
    /// files nor directories. Non-zero makes `total_bytes` a floor.
    pub unavailable_entry_count: usize,
    /// Reclaimable free-page bytes of the graph database, or `None` when it
    /// could not be sampled without waiting on a live writer.
    pub free_bytes: Option<u64>,
    /// Free pages as a fraction of total pages, or `None` when unsampled.
    pub free_page_ratio: Option<f64>,
}

/// On-disk bytes of one project store, by family.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct StoreKindBytesV1 {
    /// `tracedecay.db` with its `-wal` and `-shm`.
    pub graph_database: u64,
    /// `tracedecay.sealed/`: sealed code-graph generations.
    pub sealed_graph: u64,
    /// Completed text artifacts and each scope's text-artifact staging.
    pub text_artifacts: u64,
    /// The rest of `code-index-v1/` (generation manifests and segments,
    /// publication pointers, retention evidence) and the
    /// `tracedecay.graph-replay/` pool of retired generations.
    pub generation_artifacts: u64,
    /// `sessions.db` with its sidecars and spools.
    pub sessions: u64,
    /// Everything else: hook spools, dashboard state, locks, manifests.
    pub other: u64,
}

impl StoreKindBytesV1 {
    fn add(&mut self, relative: &Path, bytes: u64) {
        let mut components = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy());
        let first = components.next().unwrap_or_default();
        let db_filename = tracedecay_runtime_core::config::DB_FILENAME;
        let family = if first == db_filename
            || first
                .strip_prefix(db_filename)
                .is_some_and(|suffix| suffix.starts_with('-'))
        {
            &mut self.graph_database
        } else if first == SEALED_GRAPH_DIRECTORY {
            &mut self.sealed_graph
        } else if first == GRAPH_REPLAY_POOL_DIRECTORY {
            &mut self.generation_artifacts
        } else if first == CODE_INDEX_DIRECTORY {
            let second = components.next().unwrap_or_default();
            let third = components.next().unwrap_or_default();
            if second == CODE_TEXT_ARTIFACTS_DIRECTORY_V1
                || third == CODE_TEXT_ARTIFACT_STAGING_DIRECTORY_V1
            {
                &mut self.text_artifacts
            } else {
                &mut self.generation_artifacts
            }
        } else if first.starts_with(SESSIONS_DB_FILENAME)
            || first
                .strip_prefix('.')
                .is_some_and(|name| name.starts_with(SESSIONS_DB_FILENAME))
        {
            &mut self.sessions
        } else {
            &mut self.other
        };
        *family = family.saturating_add(bytes);
    }

    fn total(self) -> u64 {
        [
            self.graph_database,
            self.sealed_graph,
            self.text_artifacts,
            self.generation_artifacts,
            self.sessions,
            self.other,
        ]
        .into_iter()
        .fold(0u64, u64::saturating_add)
    }
}

/// The full report: per-store sizes plus an unregistered-directory backlog
/// summary (plan 38 §2's disjoint on-disk-only audit class, sized here rather
/// than classified, the daemon sweep and `sweep_unregistered_stores` own
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
    /// explicit `storage report` command attaches it after paging completes.
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

        // Store rows size every file under `projects/<id>/`, but profile-level
        // files beside `global.db` (the profile session store, spools, daemon
        // state) are only sized by the full census, so this total is a floor.
        let mut excluded_families =
            vec!["profile-level files outside global.db and projects/".to_owned()];
        if self.coverage.state == StorageReportCoverageState::Partial {
            excluded_families.push("registered stores beyond this page".to_owned());
        }
        let unreadable_store_entries = self.stores.iter().fold(0usize, |total, store| {
            total.saturating_add(store.unavailable_entry_count)
        });
        if unreadable_store_entries > 0 {
            excluded_families.push(format!(
                "{unreadable_store_entries} unreadable entries under registered stores"
            ));
        }
        ProfileTotalSizeV1 {
            state: ProfileTotalCoverageStateV1::Partial,
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
    /// Both are `None` for an unpublished store: sealed crash debris exists
    /// but no active publication pointer was ever written, and every
    /// generation in the store is collectable.
    pub active_generation_id: Option<String>,
    pub active_generation_file: Option<String>,
    pub vector_readable_sources: Vec<String>,
    pub superseded_generation_count: usize,
    pub superseded_generation_bytes: u64,
    pub collectable_generation_count: usize,
    pub collectable_generation_bytes: u64,
    pub collectable_generations: Vec<CodeGenerationRetentionGenerationV1>,
    /// Completed text artifacts no retained generation names, collectable by
    /// the same pass. An artifact becomes collectable once the generations
    /// naming it leave the durable index.
    pub collectable_text_artifact_count: usize,
    pub collectable_text_artifact_bytes: u64,
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
///
/// Filesystem sizing, `SQLite` family copy, and per-store probes run on the
/// blocking pool so this CLI path matches the daemon-paged sibling and does
/// not stall the async runtime.
#[tracing::instrument(name = "maintenance.storage_report.build", level = "trace", skip_all)]
pub async fn build_storage_report(
    profile_root: &Path,
) -> tracedecay_domain::errors::Result<StorageReport> {
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
    let registered_projects = if global_db_path.exists() {
        load_registered_project_rows(profile_root, &global_db_path).await?
    } else {
        Vec::new()
    };
    let profile_root_buf = profile_root.to_path_buf();
    let sampled = tokio::task::spawn_blocking(move || {
        sample_registered_storage(&profile_root_buf, &global_db_path, registered_projects)
    })
    .await
    .map_err(|error| report_error("join storage report sample", error))??;

    Ok(StorageReport {
        profile_root: profile_root.display().to_string(),
        stores: sampled.stores,
        code_generation_retention: sampled.code_generation_retention,
        code_generation_retention_availability: sampled.code_generation_retention_availability,
        unregistered_dir_count: sampled.unregistered_dir_count,
        unregistered_bytes: sampled.unregistered_bytes,
        global_db_bytes: sampled.global_db_bytes,
        full_profile_size: Some(full_profile_size),
        coverage: StorageReportCoverage::default(),
    })
}

struct SampledRegisteredStorage {
    stores: Vec<StoreSizeReportEntry>,
    code_generation_retention: Vec<CodeGenerationRetentionDryRunEntry>,
    code_generation_retention_availability: Vec<CodeGenerationRetentionAvailabilityEntry>,
    unregistered_dir_count: usize,
    unregistered_bytes: u64,
    global_db_bytes: u64,
}

/// Copies `global.db` off-profile on the blocking pool, then lists registered
/// project rows over the async snapshot reader.
async fn load_registered_project_rows(
    profile_root: &Path,
    global_db_path: &Path,
) -> tracedecay_domain::errors::Result<Vec<(String, String)>> {
    let profile_root = profile_root.to_path_buf();
    let global_db_path = global_db_path.to_path_buf();
    let (scratch, snapshot_source) = tokio::task::spawn_blocking(move || {
        let scratch = tempfile::tempdir()
            .map_err(|error| report_error("create external read-snapshot scratch", error))?;
        validate_external_scratch(&profile_root, scratch.path())
            .map_err(|error| report_error("validate read-snapshot scratch placement", error))?;
        // The live family stays untouched: a read-only SQLite open against a
        // WAL database can still materialize a missing SHM sidecar, and any
        // snapshot lock or scratch must stay outside the profile being
        // reported. The copy is a private throwaway, not a managed TraceDecay
        // database, so Foreign/read-only snapshot policy is the truthful
        // authority: Owned would demand managed-daemon or exclusive-maintenance
        // on a path that can never hold either.
        let snapshot_source = scratch.path().join(GLOBAL_DB_FILENAME);
        copy_sqlite_family(&global_db_path, &snapshot_source)
            .map_err(|error| report_error("copy global.db family for read-only report", error))?;
        Ok::<_, tracedecay_domain::errors::TraceDecayError>((scratch, snapshot_source))
    })
    .await
    .map_err(|error| report_error("join global.db family copy", error))??;
    // This clone has no live writer holding its copied SHM. Normalize only
    // the owned scratch family before snapshot validation: a SQLite reader
    // would otherwise rebuild that transient SHM and invalidate its own read.
    tracedecay_runtime_core::sqlite_read_snapshot::materialize(
        &snapshot_source,
        tracedecay_runtime_core::sqlite_read_snapshot::SnapshotReadControl::unlimited(),
    )
    .await
    .map_err(|error| report_error("materialize private global.db family", error))?;
    let snapshot = tracedecay_runtime_core::sqlite_read_snapshot::open_foreign_in(
        &snapshot_source,
        scratch.path(),
        tracedecay_runtime_core::sqlite_read_snapshot::SnapshotReadControl::unlimited(),
    )
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
    let mut projects = Vec::new();
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
        projects.push((project_id, canonical_root));
    }
    Ok(projects)
}

fn sample_registered_storage(
    profile_root: &Path,
    global_db_path: &Path,
    registered_projects: Vec<(String, String)>,
) -> tracedecay_domain::errors::Result<SampledRegisteredStorage> {
    let mut registered_ids = HashSet::new();
    let mut stores = Vec::new();
    let mut code_generation_retention = Vec::new();
    let mut code_generation_retention_availability = Vec::new();
    for (project_id, canonical_root) in registered_projects {
        registered_ids.insert(project_id.clone());
        // Offline reports have no mounted scheduler identity authority, so
        // the retention protection set is unavailable rather than inferred
        // from a path.
        append_project_report(
            profile_root,
            &project_id,
            &canonical_root,
            Err(RETENTION_PROTECTION_UNRESOLVED),
            &mut stores,
            &mut code_generation_retention,
            &mut code_generation_retention_availability,
        )?;
    }
    let (unregistered_dir_count, unregistered_bytes) =
        scan_unregistered_dirs(profile_root, &registered_ids);
    Ok(SampledRegisteredStorage {
        stores,
        code_generation_retention,
        code_generation_retention_availability,
        unregistered_dir_count,
        unregistered_bytes,
        global_db_bytes: database_family_bytes(global_db_path),
    })
}

/// Where a profile storage report page resumes: after a registered project,
/// or at an opaque position in the profile directory stream.
#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
enum StorageReportPosition {
    Projects { after_project_id: Option<String> },
    Directories { after: Option<String> },
}

/// The operation and page size a storage report continuation is minted for.
fn storage_report_binding(limit: usize) -> tracedecay_domain::errors::Result<CursorBindingV1> {
    CursorBindingV1::builder("storage_report")
        .parameter("limit", &limit)
        .build()
        .map_err(|error| TraceDecayError::Config {
            message: format!("bind storage report cursor: {error}"),
        })
}

fn storage_report_cursor(
    binding: &CursorBindingV1,
    position: &StorageReportPosition,
) -> tracedecay_domain::errors::Result<String> {
    encode_bound_cursor(binding, position).map_err(|error| TraceDecayError::Config {
        message: format!("encode storage report cursor: {error}"),
    })
}

fn storage_report_cursor_refusal(mismatch: &CursorBindingMismatchV1) -> TraceDecayError {
    TraceDecayError::project_route(mismatch.code(), false, mismatch.message())
}

/// Builds one bounded page through the daemon's retained global registry
/// authority. Registered projects and top-level profile directories are
/// separate cursor phases so neither the registry query nor the filesystem
/// census performs an unbounded profile-wide scan.
#[tracing::instrument(
    name = "maintenance.storage_report.build_page",
    level = "trace",
    skip_all
)]
pub async fn build_storage_report_page_from_registered_global_db(
    profile_root: &Path,
    global_db: &tracedecay_global_db::RegisteredGlobalDb,
    code_index_schedulers: Option<&CodeIndexSchedulerRegistryV1>,
    cursor: Option<&str>,
    limit: usize,
) -> tracedecay_domain::errors::Result<StorageReport> {
    let limit = limit.clamp(1, MAX_STORAGE_REPORT_PAGE_LIMIT);
    let global_db_bytes = database_family_bytes(&profile_root.join(GLOBAL_DB_FILENAME));
    let binding = storage_report_binding(limit)?;
    let position = match cursor {
        Some(cursor) => decode_bound_cursor::<StorageReportPosition>(&binding, cursor)
            .map_err(|mismatch| storage_report_cursor_refusal(&mismatch))?,
        None => StorageReportPosition::Projects {
            after_project_id: None,
        },
    };
    let after_directory = match position {
        StorageReportPosition::Directories { after } => after,
        StorageReportPosition::Projects { after_project_id } => {
            return project_storage_report_page(
                profile_root,
                global_db,
                code_index_schedulers,
                &binding,
                after_project_id.as_deref(),
                limit,
                global_db_bytes,
            )
            .await;
        }
    };
    let profile_root_buf = profile_root.to_path_buf();
    let directory_page = tokio::task::spawn_blocking(move || {
        list_project_directories_page(
            &profile_root_buf,
            after_directory.as_deref().unwrap_or_default(),
            limit,
        )
    })
    .await
    .map_err(|error| report_error("join storage directory page", error))??;
    let project_ids = directory_page
        .directories
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    let registered = global_db.registered_code_project_ids(&project_ids).await?;
    let unregistered = directory_page
        .directories
        .iter()
        .filter(|(name, _)| !registered.contains(name))
        .map(|(_, path)| path.clone())
        .collect::<Vec<_>>();
    let (unregistered_dir_count, unregistered_bytes) = tokio::task::spawn_blocking(move || {
        let bytes = unregistered.iter().fold(0u64, |total, path| {
            total.saturating_add(super::orphan_stores::dir_size_bytes(path))
        });
        (unregistered.len(), bytes)
    })
    .await
    .map_err(|error| report_error("join unregistered storage page", error))?;
    let next_cursor = directory_page
        .next_cursor
        .map(|after| {
            storage_report_cursor(
                &binding,
                &StorageReportPosition::Directories { after: Some(after) },
            )
        })
        .transpose()?;
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

async fn project_storage_report_page(
    profile_root: &Path,
    global_db: &tracedecay_global_db::RegisteredGlobalDb,
    code_index_schedulers: Option<&CodeIndexSchedulerRegistryV1>,
    binding: &CursorBindingV1,
    after_project_id: Option<&str>,
    limit: usize,
    global_db_bytes: u64,
) -> tracedecay_domain::errors::Result<StorageReport> {
    let mut projects = global_db
        .list_code_projects_after(after_project_id, limit.saturating_add(1))
        .await?;
    let has_more = projects.len() > limit;
    projects.truncate(limit);
    let next_position = if has_more {
        let Some(last_project) = projects.last() else {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: "project storage report page lost its continuation".to_owned(),
            });
        };
        StorageReportPosition::Projects {
            after_project_id: Some(last_project.project_id.clone()),
        }
    } else {
        StorageReportPosition::Directories { after: None }
    };
    let next_cursor = storage_report_cursor(binding, &next_position)?;
    let mut protections = Vec::with_capacity(projects.len());
    for project in &projects {
        protections.push(
            resolve_retention_protection(
                code_index_schedulers,
                global_db,
                Path::new(&project.canonical_root),
            )
            .await,
        );
    }
    let profile_root = profile_root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut report = StorageReport {
            profile_root: profile_root.display().to_string(),
            global_db_bytes,
            coverage: StorageReportCoverage::partial(next_cursor),
            ..StorageReport::default()
        };
        for (project, protection) in projects.into_iter().zip(protections) {
            append_project_report(
                &profile_root,
                &project.project_id,
                &project.canonical_root,
                protection,
                &mut report.stores,
                &mut report.code_generation_retention,
                &mut report.code_generation_retention_availability,
            )?;
        }
        Ok(report)
    })
    .await
    .map_err(|error| report_error("join daemon-backed storage report page", error))?
}

#[derive(Debug)]
struct ProjectDirectoryPage {
    directories: Vec<(String, PathBuf)>,
    next_cursor: Option<String>,
    #[cfg_attr(not(test), allow(dead_code))]
    entries_scanned: usize,
}

/// Reads one bounded page in the directory stream's native order.
///
/// The cursor is opaque: callers must return it unchanged rather than treating
/// it as a directory name or assuming lexical ordering.
fn list_project_directories_page(
    profile_root: &Path,
    cursor: &str,
    limit: usize,
) -> tracedecay_domain::errors::Result<ProjectDirectoryPage> {
    let projects_dir = profile_root.join("projects");
    match std::fs::symlink_metadata(&projects_dir) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ProjectDirectoryPage {
                directories: Vec::new(),
                next_cursor: None,
                entries_scanned: 0,
            });
        }
        Err(error) => {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("inspect storage report projects directory: {error}"),
            });
        }
    }
    let page = super::orphan_stores::read_project_directory_page(
        profile_root,
        (!cursor.is_empty()).then_some(cursor),
        limit,
        &|| false,
    )?;
    let Some(page) = page else {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: "storage report directory page was unexpectedly interrupted".to_owned(),
        });
    };
    let directories = page
        .entries
        .into_iter()
        .map(|name| {
            let path = projects_dir.join(&name);
            (name, path)
        })
        .collect::<Vec<_>>();
    Ok(ProjectDirectoryPage {
        directories,
        next_cursor: page.next_cursor,
        entries_scanned: page.entries_scanned,
    })
}

/// Builds one project-scoped report on the daemon's blocking pool so bounded
/// read-only `SQLite` samples cannot stall the async authority loop.
///
/// The retention dry run plans against the same protection set daemon
/// retention resolves: the scheduler's serving generations and the live
/// native-preview bindings. Without the scheduler authority it reports
/// `generation_retention_graph_inventory_unavailable` rather than planning
/// against an unproven protection set.
pub async fn build_project_storage_report_from_daemon(
    profile_root: &Path,
    project_id: &str,
    canonical_root: &Path,
    global_db: &tracedecay_global_db::RegisteredGlobalDb,
    code_index_schedulers: Option<&CodeIndexSchedulerRegistryV1>,
) -> tracedecay_domain::errors::Result<StorageReport> {
    let protection =
        resolve_retention_protection(code_index_schedulers, global_db, canonical_root).await;
    let profile_root = profile_root.to_path_buf();
    let project_id = project_id.to_owned();
    let canonical_root = canonical_root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        build_project_storage_report(&profile_root, &project_id, &canonical_root, protection)
    })
    .await
    .map_err(|error| report_error("join daemon-backed project storage report", error))?
}

async fn resolve_retention_protection(
    code_index_schedulers: Option<&CodeIndexSchedulerRegistryV1>,
    global_db: &tracedecay_global_db::RegisteredGlobalDb,
    canonical_root: &Path,
) -> RetentionProtection {
    let Some(schedulers) = code_index_schedulers else {
        return Err(RETENTION_PROTECTION_UNRESOLVED);
    };
    code_generation_protection(schedulers, global_db, canonical_root)
        .await
        .map(|protection| protection.sources)
        .map_err(CodeGenerationProtectionUnavailableV1::reason)
}

/// Build the same read-only report for one explicitly identified shard without
/// opening `global.db`. This is the daemon-independent path for maintenance
/// when the global registry's exclusive-maintenance authority is unavailable.
/// The protection set (the serving generation plus live native-preview
/// bindings) is only resolvable from daemon authorities; a caller that holds
/// it passes it here. Inventory-less surfaces pass the reason it is missing,
/// and the retention dry run reports itself unavailable with that reason
/// rather than planning against an unproven protection set.
#[tracing::instrument(name = "maintenance.storage_report.project", level = "trace", skip_all)]
pub fn build_project_storage_report(
    profile_root: &Path,
    project_id: &str,
    canonical_root: &Path,
    vector_readable_sources: Result<BTreeSet<CodeGenerationId>, &'static str>,
) -> tracedecay_domain::errors::Result<StorageReport> {
    tracedecay_runtime_core::storage::validate_project_id(project_id).map_err(|message| {
        tracedecay_domain::errors::TraceDecayError::Config {
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
        vector_readable_sources,
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
    vector_readable_sources: RetentionProtection,
    stores: &mut Vec<StoreSizeReportEntry>,
    code_generation_retention: &mut Vec<CodeGenerationRetentionDryRunEntry>,
    code_generation_retention_availability: &mut Vec<CodeGenerationRetentionAvailabilityEntry>,
) -> tracedecay_domain::errors::Result<()> {
    let data_root = profile_root.join("projects").join(project_id);
    if data_root.is_dir() {
        let mut kinds = StoreKindBytesV1::default();
        let unavailable_entry_count = walk_regular_files(&data_root, &mut |path, bytes| {
            if let Ok(relative) = path.strip_prefix(&data_root) {
                kinds.add(relative, bytes);
            }
        });
        let (free_bytes, free_page_ratio) =
            sample_free_pages(&data_root.join(tracedecay_runtime_core::config::DB_FILENAME))
                .map_or((None, None), |(free_bytes, ratio)| {
                    (Some(free_bytes), Some(ratio))
                });
        stores.push(StoreSizeReportEntry {
            project_id: project_id.to_owned(),
            canonical_root: canonical_root.to_owned(),
            total_bytes: kinds.total(),
            kinds,
            unavailable_entry_count,
            free_bytes,
            free_page_ratio,
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
    // The serving generation and live native-preview bindings are only
    // provable from daemon authorities. Without them the dry run is reported
    // unavailable rather than planned against an empty protection set.
    let readable_sources = match vector_readable_sources {
        Ok(sources) => sources,
        Err(reason) => {
            code_generation_retention_availability.push(CodeGenerationRetentionAvailabilityEntry {
                project_id: project_id.to_owned(),
                store_root: code_index_store_root.display().to_string(),
                state: StorageReportAvailabilityState::Unavailable,
                reason: Some(reason.to_owned()),
            });
            return Ok(());
        }
    };
    let plan = match plan_code_generation_retention_with_verification(
        &code_index_store_root,
        &readable_sources,
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
        active_generation_id: plan
            .active_generation_id
            .as_ref()
            .map(|generation| generation.as_str().to_owned()),
        active_generation_file: plan.active_generation_file().map(str::to_owned),
        vector_readable_sources: plan
            .vector_readable_sources
            .iter()
            .map(|source| source.as_str().to_owned())
            .collect(),
        superseded_generation_count: plan.superseded_generations.len(),
        superseded_generation_bytes: plan.superseded_generation_bytes(),
        collectable_generation_count: plan.collectable_generations.len(),
        collectable_generation_bytes: plan.collectable_generation_bytes(),
        collectable_text_artifact_count: plan.collectable_text_artifact_count(),
        collectable_text_artifact_bytes: plan.collectable_text_artifact_bytes(),
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
) -> tracedecay_domain::errors::Result<bool> {
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
    let generation =
        tracedecay_runtime_core::sqlite_read_snapshot::SourceGeneration::capture(source)?;
    for suffix in ["", "-wal", "-shm"] {
        let source_member = sqlite_family_member(source, suffix);
        if !source_member.is_file() {
            continue;
        }
        std::fs::copy(&source_member, sqlite_family_member(destination, suffix))?;
    }
    generation.validate()
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
/// Returns `None` on any failure, a store held by a busy writer, a corrupt
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
        if tracedecay_runtime_core::storage::validate_project_id(&name).is_err()
            || registered_ids.contains(&name)
        {
            continue;
        }
        count += 1;
        // Shares the orphan sweep's walker so the bytes this report shows for
        // a backlog directory are the same bytes that sweep would reclaim.
        bytes = bytes.saturating_add(super::orphan_stores::dir_size_bytes(&entry.path()));
    }
    (count, bytes)
}

/// Visit every regular file under `root` with its byte length, without
/// following symlinks, and return how many entries could not be read or were
/// neither regular files nor directories. Those make any total built from the
/// visits a lower bound instead of a successful zero-size family.
fn walk_regular_files(root: &Path, visit: &mut dyn FnMut(&Path, u64)) -> usize {
    let mut unavailable_entry_count = 0usize;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            unavailable_entry_count = unavailable_entry_count.saturating_add(1);
            continue;
        };
        for entry in entries {
            let Ok(entry) = entry else {
                unavailable_entry_count = unavailable_entry_count.saturating_add(1);
                continue;
            };
            let Ok(file_type) = entry.file_type() else {
                unavailable_entry_count = unavailable_entry_count.saturating_add(1);
                continue;
            };
            // A socket (the running daemon's endpoint) holds no bytes.
            #[cfg(unix)]
            if std::os::unix::fs::FileTypeExt::is_socket(&file_type) {
                continue;
            }
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file() {
                match entry.metadata() {
                    Ok(metadata) => visit(&entry.path(), metadata.len()),
                    Err(_) => {
                        unavailable_entry_count = unavailable_entry_count.saturating_add(1);
                    }
                }
            } else {
                unavailable_entry_count = unavailable_entry_count.saturating_add(1);
            }
        }
    }
    unavailable_entry_count
}

/// Count every regular file under a profile root without following symlinks.
/// Failures remain visible as a partial lower bound instead of a successful
/// zero-size family.
#[tracing::instrument(
    name = "maintenance.storage_report.scan_profile_size",
    level = "trace",
    skip_all
)]
pub fn scan_full_profile_size(profile_root: &Path) -> FullProfileSizeV1 {
    let mut total_bytes = 0u64;
    let unavailable_entry_count = walk_regular_files(profile_root, &mut |_, bytes| {
        total_bytes = total_bytes.saturating_add(bytes);
    });
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
) -> tracedecay_domain::errors::TraceDecayError {
    tracedecay_domain::errors::TraceDecayError::Database {
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
    use tracedecay_runtime_core::db::engine::TestConnection;

    fn store(project_id: &str, total_bytes: u64) -> StoreSizeReportEntry {
        StoreSizeReportEntry {
            project_id: project_id.to_owned(),
            canonical_root: format!("/work/{project_id}"),
            total_bytes,
            kinds: StoreKindBytesV1::default(),
            unavailable_entry_count: 0,
            free_bytes: None,
            free_page_ratio: None,
        }
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
    fn unreadable_store_entries_are_named_rather_than_silently_dropped() {
        let mut unreadable = store("alpha", 400);
        unreadable.unavailable_entry_count = 2;
        let report = StorageReport {
            stores: vec![unreadable],
            global_db_bytes: 100,
            ..StorageReport::default()
        };

        let total = report.profile_total_size();
        assert_eq!(total.state, ProfileTotalCoverageStateV1::Partial);
        assert_eq!(total.accounted_bytes, 500);
        assert_eq!(
            total.excluded_families,
            vec![
                "profile-level files outside global.db and projects/".to_owned(),
                "2 unreadable entries under registered stores".to_owned(),
            ]
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
                tracedecay_runtime_core::db::engine::params![*project_id, *canonical_root],
            )
            .await
            .unwrap();
        }
    }

    fn seed_graph_db(profile_root: &Path, project_id: &str) {
        let data_root = profile_root.join("projects").join(project_id);
        std::fs::create_dir_all(&data_root).unwrap();
        let connection = rusqlite::Connection::open(
            data_root.join(tracedecay_runtime_core::config::DB_FILENAME),
        )
        .unwrap();
        connection
            .execute_batch("CREATE TABLE fixture (id INTEGER PRIMARY KEY);")
            .unwrap();
    }

    /// Inventory positions survive directory mutation without repeating records.
    #[test]
    fn project_directory_pages_cover_every_entry_within_a_bounded_pass() {
        for page_size in [64, 256] {
            let tmp = tempfile::TempDir::new().unwrap();
            let profile_root = tmp.path().join("profile");
            let projects_dir = profile_root.join("projects");
            let expected = (0..513)
                .map(|index| format!("proj_{index:04}"))
                .collect::<BTreeSet<_>>();
            for name in &expected {
                std::fs::create_dir_all(projects_dir.join(name)).unwrap();
            }

            let mut cursor = String::new();
            let mut observed = BTreeSet::new();
            let mut entries_scanned = 0usize;
            let mut returned = 0usize;
            let mut first_page = true;
            loop {
                let page =
                    list_project_directories_page(&profile_root, &cursor, page_size).unwrap();
                entries_scanned = entries_scanned.saturating_add(page.entries_scanned);
                for (name, _) in page.directories {
                    returned = returned.saturating_add(1);
                    observed.insert(name);
                }

                let Some(next_cursor) = page.next_cursor else {
                    break;
                };
                cursor = next_cursor;
                if first_page {
                    first_page = false;
                    std::fs::create_dir_all(projects_dir.join("proj_foreign_added")).unwrap();
                    std::fs::remove_dir(projects_dir.join("proj_0500")).unwrap();
                }
            }

            let expected_without_removed = expected
                .iter()
                .filter(|name| name.as_str() != "proj_0500")
                .cloned()
                .collect::<BTreeSet<_>>();
            assert!(
                observed.is_superset(&expected_without_removed),
                "page size {page_size} skipped an original directory"
            );
            assert!(
                returned == observed.len(),
                "page size {page_size} returned {returned} entries for {} directories",
                expected.len()
            );
            assert!(
                entries_scanned <= (expected.len() + 1).saturating_mul(2),
                "page size {page_size} rescanned directory or inventory entries: {entries_scanned}"
            );
            assert!(
                entries_scanned >= observed.len(),
                "entry accounting must cover every returned directory"
            );
        }
    }

    #[test]
    fn project_directory_report_resume_is_stable_after_mutation_and_repeated_requests() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_root = tmp.path().join("profile");
        let projects = profile_root.join("projects");
        for index in 0..17 {
            std::fs::create_dir_all(projects.join(format!("proj_{index:02}"))).unwrap();
        }
        let first = list_project_directories_page(&profile_root, "", 2).unwrap();
        let cursor = first.next_cursor.unwrap();
        for (_, path) in &first.directories {
            std::fs::remove_dir(path).unwrap();
        }
        std::fs::create_dir_all(projects.join("proj_added")).unwrap();
        let resumed = list_project_directories_page(&profile_root, &cursor, 2).unwrap();
        let repeated = list_project_directories_page(&profile_root, &cursor, 2).unwrap();
        assert_eq!(resumed.directories, repeated.directories);
        assert_eq!(resumed.next_cursor, repeated.next_cursor);
        let mut observed = first
            .directories
            .into_iter()
            .map(|(name, _)| name)
            .collect::<BTreeSet<_>>();
        let mut cursor = Some(cursor);
        let mut pages = 0;
        while let Some(saved) = cursor {
            let page = list_project_directories_page(&profile_root, &saved, 2).unwrap();
            for (name, _) in page.directories {
                assert!(
                    observed.insert(name),
                    "resumption repeated an inventory record"
                );
            }
            cursor = page.next_cursor;
            pages += 1;
            assert!(pages <= 10, "resumption failed to converge");
        }
        assert!((0..17).all(|index| observed.contains(&format!("proj_{index:02}"))));
    }

    #[tokio::test]
    async fn project_directory_storage_report_counts_stable_resume_and_rejects_invalid_position() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_root = tmp.path();
        let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::profile(
            profile_root,
        )
        .await
        .unwrap();
        let db = runtime.profile_database_arc();
        for index in 0..3 {
            let project = profile_root
                .join("projects")
                .join(format!("proj_report_{index}"));
            std::fs::create_dir_all(&project).unwrap();
            std::fs::write(project.join("payload"), b"1234").unwrap();
        }
        let registry =
            build_storage_report_page_from_registered_global_db(profile_root, &db, None, None, 1)
                .await
                .unwrap();
        assert_eq!(registry.stores.len(), 0);
        let directories = registry.coverage.next_cursor.unwrap();
        assert!(directories.starts_with("bc1."), "{directories}");
        let first = build_storage_report_page_from_registered_global_db(
            profile_root,
            &db,
            None,
            Some(&directories),
            1,
        )
        .await
        .unwrap();
        assert_eq!(
            (first.unregistered_dir_count, first.unregistered_bytes),
            (1, 4)
        );
        let cursor = first.coverage.next_cursor.unwrap();
        let second = build_storage_report_page_from_registered_global_db(
            profile_root,
            &db,
            None,
            Some(&cursor),
            1,
        )
        .await
        .unwrap();
        let repeated = build_storage_report_page_from_registered_global_db(
            profile_root,
            &db,
            None,
            Some(&cursor),
            1,
        )
        .await
        .unwrap();
        assert_eq!(
            (second.unregistered_dir_count, second.unregistered_bytes),
            (1, 4)
        );
        assert_eq!(second.coverage.next_cursor, repeated.coverage.next_cursor);
        assert_eq!(second.unregistered_bytes, repeated.unregistered_bytes);
        for (presented, limit, code, message) in [
            (
                cursor.as_str(),
                2,
                "cursor.parameter_changed",
                "The cursor was issued for a request with a different `limit`. Repeat the \
                 request with the parameters that returned the cursor, or restart without it.",
            ),
            (
                "directories:portable-v2:0",
                1,
                "cursor.invalid",
                "The cursor was not issued by this operation. Restart without it.",
            ),
        ] {
            let refused = build_storage_report_page_from_registered_global_db(
                profile_root,
                &db,
                None,
                Some(presented),
                limit,
            )
            .await
            .unwrap_err();
            assert_eq!(
                refused.project_route_context(),
                Some((code, false, message))
            );
        }
        let third = build_storage_report_page_from_registered_global_db(
            profile_root,
            &db,
            None,
            second.coverage.next_cursor.as_deref(),
            1,
        )
        .await
        .unwrap();
        assert_eq!(
            (third.unregistered_dir_count, third.unregistered_bytes),
            (1, 4)
        );
        let exhausted = build_storage_report_page_from_registered_global_db(
            profile_root,
            &db,
            None,
            third.coverage.next_cursor.as_deref(),
            1,
        )
        .await
        .unwrap();
        assert_eq!(
            (
                exhausted.unregistered_dir_count,
                exhausted.unregistered_bytes
            ),
            (0, 0)
        );
        assert!(exhausted.coverage.next_cursor.is_none());
    }

    #[test]
    fn project_directory_report_rejects_invalidated_inventory_positions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_root = tmp.path().join("profile");
        for index in 0..3 {
            std::fs::create_dir_all(profile_root.join("projects").join(format!("proj_{index}")))
                .unwrap();
        }
        let first = list_project_directories_page(&profile_root, "", 1).unwrap();
        let cursor = first.next_cursor.unwrap();
        let (prefix, _) = cursor.rsplit_once(':').unwrap();
        for offset in [1, u64::MAX] {
            let error =
                list_project_directories_page(&profile_root, &format!("{prefix}:{offset}"), 1)
                    .unwrap_err();
            assert!(matches!(
                error,
                tracedecay_domain::errors::TraceDecayError::Config { .. }
            ));
        }
        // Rejected positions must not modify the inventory or poison its valid continuation.
        let resumed = list_project_directories_page(&profile_root, &cursor, 1).unwrap();
        assert_eq!(resumed.directories.len(), 1);
        assert_ne!(first.directories, resumed.directories);
    }

    #[test]
    fn project_directory_report_empty_locked_page_retains_continuation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_root = tmp.path().join("profile");
        let projects = profile_root.join("projects");
        std::fs::create_dir_all(projects.join("proj_pending")).unwrap();
        let metadata = projects.metadata().unwrap();
        let modified = metadata
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        let signature = format!(
            "{}-{}-{}",
            modified.as_secs(),
            modified.subsec_nanos(),
            metadata.len()
        );
        let inventory = profile_root
            .join("maintenance/unregistered-project-directory-inventory-v2")
            .join(format!("{signature}.log"));
        std::fs::create_dir_all(inventory.parent().unwrap()).unwrap();
        let lock = tracedecay_runtime_core::storage::try_acquire_sidecar_lock(
            &tracedecay_runtime_core::storage::append_lock_path(&inventory),
        )
        .unwrap()
        .unwrap();
        let empty = list_project_directories_page(&profile_root, "", 1).unwrap();
        assert_eq!(empty.directories.len(), 0);
        let cursor = empty
            .next_cursor
            .expect("lock contention is incomplete, never empty success");
        let repeated = list_project_directories_page(&profile_root, &cursor, 1).unwrap();
        assert_eq!(repeated.directories.len(), 0);
        assert_eq!(repeated.next_cursor.as_deref(), Some(cursor.as_str()));
        drop(lock);
        let resumed = list_project_directories_page(&profile_root, &cursor, 1).unwrap();
        assert_eq!(resumed.directories[0].0, "proj_pending");
        let final_page = list_project_directories_page(
            &profile_root,
            resumed.next_cursor.as_deref().unwrap(),
            1,
        )
        .unwrap();
        assert_eq!(final_page.directories.len(), 0);
        assert!(final_page.next_cursor.is_none());
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
    async fn storage_report_preserves_the_exact_live_global_database_family() {
        for checkpointed in [false, true] {
            let tmp = tempfile::TempDir::new().unwrap();
            let profile_root = tmp.path().join("profile");
            std::fs::create_dir_all(&profile_root).unwrap();
            let global_db = profile_root.join(GLOBAL_DB_FILENAME);
            let writer = rusqlite::Connection::open(&global_db).unwrap();
            writer
                .execute_batch(
                    "PRAGMA journal_mode = WAL;
                     PRAGMA wal_autocheckpoint = 0;
                     CREATE TABLE code_projects (
                         project_id TEXT PRIMARY KEY, canonical_root TEXT NOT NULL
                     );
                     INSERT INTO code_projects VALUES ('proj_a', '/repos/a');",
                )
                .unwrap();
            if checkpointed {
                writer
                    .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
                    .unwrap();
            }
            let wal = sqlite_family_member(&global_db, "-wal");
            assert_eq!(wal.metadata().unwrap().len() == 0, checkpointed);
            assert!(sqlite_family_member(&global_db, "-shm").is_file());
            seed_graph_db(&profile_root, "proj_a");
            let before = sqlite_family_bytes(&global_db);

            let report = build_storage_report(&profile_root).await.unwrap();

            assert_eq!(report.stores.len(), 1);
            assert_eq!(report.stores[0].project_id, "proj_a");
            assert_eq!(report.stores[0].canonical_root, "/repos/a");
            assert_eq!(
                sqlite_family_bytes(&global_db),
                before,
                "reporting must never create, change, or delete a source SQLite family member"
            );
            drop(writer);
            let closed = sqlite_family_bytes(&global_db);
            assert!(!sqlite_family_member(&global_db, "-wal").exists());
            assert!(!sqlite_family_member(&global_db, "-shm").exists());
            let report = build_storage_report(&profile_root).await.unwrap();
            assert_eq!(report.stores.len(), 1);
            assert_eq!(sqlite_family_bytes(&global_db), closed);
        }
    }

    #[test]
    fn scratch_validation_rejects_a_transient_directory_inside_the_profile() {
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("profile");
        let transient = profile.join("transient");
        std::fs::create_dir_all(&transient).unwrap();

        let rejected = validate_external_scratch(&profile, &transient).unwrap_err();
        assert_eq!(rejected.kind(), std::io::ErrorKind::Other);
        assert_eq!(
            rejected.to_string(),
            "read-snapshot scratch must be outside the inspected profile"
        );
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        assert_eq!(validate_external_scratch(&profile, &outside).unwrap(), ());
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
            .join(tracedecay_runtime_core::config::DB_FILENAME);
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
        std::fs::write(
            data_root.join(tracedecay_runtime_core::config::DB_FILENAME),
            vec![9u8; 4096],
        )
        .unwrap();

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

    /// Each store row sizes every family under its project directory, and the
    /// families add up to the same bytes a plain walk of the directory finds.
    #[test]
    fn store_row_sizes_every_store_family_under_the_project_directory() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_root = tmp.path().join("profile");
        let data_root = profile_root.join("projects").join("proj_a");
        let scope = data_root.join("code-index-v1").join("a".repeat(64));
        for (relative, bytes) in [
            ("tracedecay.db", 4_096),
            ("tracedecay.db-wal", 100),
            ("tracedecay.sealed/digest/generation.grafeo", 3_000),
            (
                "code-index-v1/code-text-artifacts-v1/text-artifact.bin",
                2_000,
            ),
            (
                "code-index-v1/code-generation-segments-v1/segment.json",
                300,
            ),
            ("tracedecay.graph-replay/generation.json", 40),
            ("sessions.db", 900),
            ("sessions.db-wal", 10),
            (".sessions.db.host-admission/claim", 5),
            ("hook-delivery-spool/claude/record", 7),
        ] {
            let path = data_root.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, vec![0u8; bytes]).unwrap();
        }
        for (relative, bytes) in [
            ("code-text-artifact-staging-v1/.text-artifact.staging", 50),
            ("code-generations-v1/generation.json", 700),
        ] {
            let path = scope.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, vec![0u8; bytes]).unwrap();
        }

        // The row sizes the store as supplied, before the free-page probe
        // opens its database.
        let walked_bytes = profile_tree_bytes(&data_root);
        let report = build_project_storage_report(
            &profile_root,
            "proj_a",
            Path::new("/repos/a"),
            Err(RETENTION_PROTECTION_UNRESOLVED),
        )
        .unwrap();

        assert_eq!(report.stores.len(), 1);
        let store = &report.stores[0];
        assert_eq!(
            store.kinds,
            StoreKindBytesV1 {
                graph_database: 4_196,
                sealed_graph: 3_000,
                text_artifacts: 2_050,
                generation_artifacts: 1_040,
                sessions: 915,
                other: 7,
            }
        );
        assert_eq!(store.total_bytes, 11_208);
        assert_eq!(store.total_bytes, walked_bytes);
        assert_eq!(store.unavailable_entry_count, 0);
    }

    /// The daemon page the CLI walks lists the collectable backlog against the
    /// protection set the scheduler registry resolves; without that authority
    /// the same store reports the backlog unavailable.
    #[tokio::test]
    async fn daemon_report_page_lists_the_collectable_generation_backlog() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_root = tmp.path().join("profile");
        let checkout = tmp.path().join("checkout");
        std::fs::create_dir_all(&checkout).unwrap();
        let checkout = tracedecay_runtime_core::path_safety::canonical_root_identity(&checkout);
        let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::profile(
            &profile_root,
        )
        .await
        .unwrap();
        let db = runtime.profile_database_arc();
        let project = db
            .upsert_code_project("proj_backlog", &checkout, None, None, None)
            .await
            .unwrap();
        let store_root =
            tracedecay_code_index_retention::code_index_generations::code_index_store_root(
                &profile_root.join("projects").join(&project.project_id),
                &checkout,
            );
        let generations =
            tracedecay_code_index_retention::code_index_generations::fixture::write_generation_store_fixture(
                &store_root,
                4,
            );
        let orphan_text = b"text artifact no retained generation names";
        let orphan_root =
            tracedecay_code_index_retention::code_index_generations::code_text_artifacts_root(
                &store_root,
            );
        std::fs::create_dir_all(&orphan_root).unwrap();
        std::fs::write(
            orphan_root.join(format!(
                "text-artifact-{}.bin",
                tracedecay_domain::canonical_text::encode_lowercase_hex(
                    &<sha2::Sha256 as sha2::Digest>::digest(orphan_text)
                )
            )),
            orphan_text,
        )
        .unwrap();
        let schedulers = crate::store_maintenance::registered_tests::unmounted_scheduler_registry();

        let page = build_storage_report_page_from_registered_global_db(
            &profile_root,
            &db,
            Some(&schedulers),
            None,
            8,
        )
        .await
        .unwrap();

        assert_eq!(page.code_generation_retention.len(), 1);
        let backlog = &page.code_generation_retention[0];
        assert_eq!(backlog.project_id, "proj_backlog");
        assert_eq!(
            backlog.active_generation_id.as_deref(),
            Some(generations[3].id.as_str())
        );
        assert_eq!(backlog.superseded_generation_count, 3);
        assert_eq!(backlog.collectable_generation_count, 3);
        assert_eq!(
            backlog
                .collectable_generations
                .iter()
                .map(|generation| generation.generation_file.clone())
                .collect::<Vec<_>>(),
            generations[..3]
                .iter()
                .map(|generation| generation.file.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            backlog.collectable_generation_bytes,
            generations[..3]
                .iter()
                .map(|generation| generation.size_bytes)
                .sum::<u64>()
        );
        assert_eq!(backlog.collectable_text_artifact_count, 1);
        assert_eq!(backlog.collectable_text_artifact_bytes, 42);
        assert_eq!(
            page.code_generation_retention_availability[0].state,
            StorageReportAvailabilityState::Available
        );

        let without_authority =
            build_storage_report_page_from_registered_global_db(&profile_root, &db, None, None, 8)
                .await
                .unwrap();
        assert_eq!(without_authority.code_generation_retention.len(), 0);
        assert_eq!(
            without_authority.code_generation_retention_availability[0]
                .reason
                .as_deref(),
            Some(RETENTION_PROTECTION_UNRESOLVED)
        );
    }

    /// The daemon's socket lives in the profile it serves; it holds no bytes
    /// and must not turn a readable census into a partial one.
    #[cfg(unix)]
    #[test]
    fn full_profile_census_is_complete_beside_a_live_daemon_socket() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_root = tmp.path().join("profile");
        std::fs::create_dir_all(profile_root.join("projects/proj_a")).unwrap();
        std::fs::write(profile_root.join("global.db"), vec![0u8; 300]).unwrap();
        std::fs::write(
            profile_root.join("projects/proj_a/tracedecay.db"),
            vec![0u8; 700],
        )
        .unwrap();
        let _socket =
            std::os::unix::net::UnixListener::bind(profile_root.join("daemon.sock")).unwrap();

        assert_eq!(
            scan_full_profile_size(&profile_root),
            FullProfileSizeV1 {
                state: ProfileTotalCoverageStateV1::Complete,
                total_bytes: 1_000,
                unavailable_entry_count: 0,
            }
        );
    }

    #[test]
    fn targeted_project_report_bypasses_global_registry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_root = tmp.path().join("profile");
        std::fs::create_dir_all(&profile_root).unwrap();
        seed_graph_db(&profile_root, "proj_a");

        let report = build_project_storage_report(
            &profile_root,
            "proj_a",
            Path::new("/repos/a"),
            Err(RETENTION_PROTECTION_UNRESOLVED),
        )
        .unwrap();

        assert_eq!(report.stores.len(), 1);
        assert_eq!(report.stores[0].project_id, "proj_a");
        assert_eq!(report.stores[0].canonical_root, "/repos/a");
        assert_eq!(report.code_generation_retention.len(), 0);
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

        let report = build_project_storage_report(
            &profile_root,
            "proj_a",
            canonical_root,
            Err(RETENTION_PROTECTION_UNRESOLVED),
        )
        .unwrap();

        assert_eq!(report.code_generation_retention.len(), 0);
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
    /// fallback still refuses, but it refuses for the *pointer*, proving the
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

        let report = build_project_storage_report(
            &profile_root,
            "proj_a",
            canonical_root,
            Ok(BTreeSet::new()),
        )
        .unwrap();
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
}
