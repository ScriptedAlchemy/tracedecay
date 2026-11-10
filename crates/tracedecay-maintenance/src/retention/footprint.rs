//! Profile footprint census: one metadata-only walk of a profile root that
//! sizes every store family against a bound derived from the store's purpose.
//!
//! The walk stats entries and never reads file contents or follows symlinks,
//! so its cost scales with the number of directory entries, not with bytes.
//! That is why the ceiling is an entry count: a byte budget small enough to be
//! cheap is smaller than one real graph container.

use std::borrow::Cow;
use std::collections::BTreeMap;
#[cfg(unix)]
use std::collections::BTreeSet;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracedecay_contracts::doctor::{
    DoctorCoverageCompletenessV1, DoctorCoverageStatementV1, DoctorEvidenceRefV1,
    DoctorEvidenceReferenceV1, DoctorEvidenceStateV1, DoctorFindingFamilyV1, DoctorFindingV1,
    DoctorStorageFamilyReadV1, DoctorStorageFindingKindV1, DoctorStorageFindingV1,
    storage_family_read,
};
use tracedecay_domain::canonical_text::sha256_hex;
use tracedecay_global_db::registry_maintenance::{RootLivenessV1, probe_root};
use tracedecay_runtime_core::text::format_bytes;

use super::storage_report::{
    FullProfileSizeV1, ProfileTotalCoverageStateV1, StoreKindBytesV1, graph_container_stem,
};

/// Entries a footprint walk visits before it stops and reports a floor.
pub const MAX_FOOTPRINT_ENTRIES: usize = 500_000;

const GIB: u64 = 1024 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;

/// Bound for the whole profile, overridden by the `profile` budget key.
pub const PROFILE_TOTAL_BOUND_BYTES: u64 = 64 * GIB;
/// Footprint key of the profile total and its budget override.
pub const PROFILE_TOTAL_KEY: &str = "profile";
const PROJECTS_DIRECTORY: &str = "projects";

/// What a profile entry holds, which decides its default bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileFamilyV1 {
    ProjectStore,
    GraphContainer,
    Wal,
    ProfileRegistry,
    UserSessions,
    UserMemory,
    LcmPayloads,
    HookAnalytics,
    ResponseHandles,
    Maintenance,
    SqliteTempSpill,
    Other,
}

/// The default bound for one family, before any owner override.
#[must_use]
pub const fn default_bound_bytes(family: ProfileFamilyV1) -> u64 {
    match family {
        // One repository's graph database, sessions shard, and the serving
        // plus one superseded sealed generation of about 1 GiB each, with
        // headroom for text artifacts and the replay pool.
        ProfileFamilyV1::ProjectStore => 10 * GIB,
        // Opening a container replays its whole block log into heap, so a
        // container past this size is resident memory the daemon cannot shed.
        ProfileFamilyV1::GraphContainer => 4 * GIB,
        // `journal_size_limit` truncates a reset WAL to the soft limit, so a
        // steady-state WAL is at or below it. Four times the limit means
        // checkpoints were blocked across several write bursts.
        ProfileFamilyV1::Wal => 4 * tracedecay_store::WAL_SOFT_LIMIT_BYTES,
        // The profile registry holds project rows, aliases, analytics, and
        // the observation authority for every project on the machine.
        ProfileFamilyV1::ProfileRegistry => 4 * GIB,
        // Every host session's raw messages for the retention window.
        ProfileFamilyV1::UserSessions => 8 * GIB,
        // Durable facts and their indexes; curated, not append-only.
        ProfileFamilyV1::UserMemory => GIB,
        // Large session payloads spilled out of the session store, retained
        // for the same window as the messages that reference them.
        ProfileFamilyV1::LcmPayloads => 16 * GIB,
        // An append-only analytics log; past this it is unrotated history.
        ProfileFamilyV1::HookAnalytics => 256 * MIB,
        // Transient response handles that expire with their sessions.
        ProfileFamilyV1::ResponseHandles => GIB,
        // Maintenance cursors, inventories, and receipts only.
        ProfileFamilyV1::Maintenance => 64 * MIB,
        // A healthy daemon's TEMP databases and sort spills are tens of
        // megabytes; past this a statement sorts bulk rows without an index.
        ProfileFamilyV1::SqliteTempSpill => GIB,
        // An entry no TraceDecay family owns.
        ProfileFamilyV1::Other => GIB,
    }
}

/// Liveness of the root a project store was registered for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectRootStateV1 {
    Present,
    Missing,
    Unverifiable,
    /// The store directory has no registry row.
    Unregistered,
}

impl ProjectRootStateV1 {
    #[must_use]
    pub fn probe(registered_root: Option<&str>) -> Self {
        match registered_root.map(|root| probe_root(Path::new(root))) {
            None => Self::Unregistered,
            Some(RootLivenessV1::Live) => Self::Present,
            Some(RootLivenessV1::Absent) => Self::Missing,
            Some(RootLivenessV1::Unverifiable) => Self::Unverifiable,
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Present => "root present",
            Self::Missing => "root missing on disk",
            Self::Unverifiable => "root unverifiable",
            Self::Unregistered => "no registry row",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectStoreRootV1 {
    pub registered_root: Option<String>,
    pub state: ProjectRootStateV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileFootprintEntryV1 {
    pub key: String,
    pub family: ProfileFamilyV1,
    pub bytes: u64,
    /// Unreadable or non-regular entries under this key; non-zero makes
    /// `bytes` a floor.
    pub unavailable_entry_count: usize,
    pub bound_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectStoreRootV1>,
    /// The project store's bytes by store family.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<StoreKindBytesV1>,
}

/// Every profile entry with its bound. Graph container and WAL entries inside
/// a project store overlap that store's entry, so the profile total is the sum
/// of walked files, never the sum of entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileFootprintV1 {
    pub entries: Vec<ProfileFootprintEntryV1>,
    pub total_bytes: u64,
    pub unavailable_entry_count: usize,
    /// The walk stopped at its entry ceiling; every total is a floor.
    pub truncated: bool,
    pub total_bound_bytes: u64,
}

impl ProfileFootprintV1 {
    #[must_use]
    pub fn full_profile_size(&self) -> FullProfileSizeV1 {
        FullProfileSizeV1 {
            state: if self.unavailable_entry_count == 0 && !self.truncated {
                ProfileTotalCoverageStateV1::Complete
            } else {
                ProfileTotalCoverageStateV1::Partial
            },
            total_bytes: self.total_bytes,
            unavailable_entry_count: self.unavailable_entry_count,
        }
    }
}

/// One entry a metadata walk visited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkedEntryV1 {
    File {
        bytes: u64,
    },
    /// Could not be read, or is neither a regular file nor a directory.
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkInterruptionV1 {
    Cancelled,
    DeadlineExceeded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkTerminationV1 {
    Complete,
    EntryBudgetExceeded,
    Cancelled,
    DeadlineExceeded,
}

/// Visit every regular file under `root` with its byte length, without
/// following symlinks or reading contents. Stops after `entry_ceiling`
/// directory entries and returns whether it did, which makes any total built
/// from the visits a floor.
pub fn walk_regular_files(
    root: &Path,
    entry_ceiling: usize,
    visit: &mut dyn FnMut(&Path, WalkedEntryV1),
) -> bool {
    let mut remaining_entries = entry_ceiling;
    walk_regular_files_with_budget(root, &mut remaining_entries, &|| None, visit)
        == WalkTerminationV1::EntryBudgetExceeded
}

/// Walk one subtree using a caller-owned entry budget. Sharing
/// `remaining_entries` across roots bounds the whole request rather than
/// restarting the limit for each project. The interruption callback is checked
/// on the blocking worker before every directory entry is inspected.
pub fn walk_regular_files_with_budget(
    root: &Path,
    remaining_entries: &mut usize,
    interruption: &dyn Fn() -> Option<WalkInterruptionV1>,
    visit: &mut dyn FnMut(&Path, WalkedEntryV1),
) -> WalkTerminationV1 {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        if let Some(interruption) = interruption() {
            return match interruption {
                WalkInterruptionV1::Cancelled => WalkTerminationV1::Cancelled,
                WalkInterruptionV1::DeadlineExceeded => WalkTerminationV1::DeadlineExceeded,
            };
        }
        // read_dir follows a symlink when the directory argument itself is
        // a symlink. Check every pending directory, including the caller's
        // root, instead of relying only on child DirEntry::file_type.
        match std::fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            _ => {
                visit(&directory, WalkedEntryV1::Unavailable);
                continue;
            }
        }
        let Ok(entries) = std::fs::read_dir(&directory) else {
            visit(&directory, WalkedEntryV1::Unavailable);
            continue;
        };
        for entry in entries {
            if let Some(interruption) = interruption() {
                return match interruption {
                    WalkInterruptionV1::Cancelled => WalkTerminationV1::Cancelled,
                    WalkInterruptionV1::DeadlineExceeded => WalkTerminationV1::DeadlineExceeded,
                };
            }
            if *remaining_entries == 0 {
                return WalkTerminationV1::EntryBudgetExceeded;
            }
            *remaining_entries -= 1;
            let Ok(entry) = entry else {
                visit(&directory, WalkedEntryV1::Unavailable);
                continue;
            };
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                visit(&path, WalkedEntryV1::Unavailable);
                continue;
            };
            // A socket (the running daemon's endpoint) holds no bytes.
            #[cfg(unix)]
            if std::os::unix::fs::FileTypeExt::is_socket(&file_type) {
                continue;
            }
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file() {
                match std::fs::symlink_metadata(&path) {
                    Ok(metadata) if metadata.is_file() => visit(
                        &path,
                        WalkedEntryV1::File {
                            bytes: metadata.len(),
                        },
                    ),
                    _ => visit(&path, WalkedEntryV1::Unavailable),
                }
            } else {
                visit(&path, WalkedEntryV1::Unavailable);
            }
        }
    }
    WalkTerminationV1::Complete
}

/// Census of every regular file under `profile_root`, bucketed by footprint
/// key. `registered_roots` maps project id to its registered canonical root;
/// `bound_overrides` replaces a default bound by exact footprint key, or the
/// profile total by `profile`.
#[tracing::instrument(name = "maintenance.footprint.census", level = "trace", skip_all)]
#[must_use]
pub fn census_profile_footprint(
    profile_root: &Path,
    registered_roots: &BTreeMap<String, String>,
    bound_overrides: &BTreeMap<String, u64>,
) -> ProfileFootprintV1 {
    let mut remaining_entries = MAX_FOOTPRINT_ENTRIES;
    census_profile_footprint_with_budget(
        profile_root,
        registered_roots,
        bound_overrides,
        &mut remaining_entries,
    )
}

#[derive(Default)]
struct EntryTally {
    bytes: u64,
    unavailable_entry_count: usize,
    kinds: Option<StoreKindBytesV1>,
    /// An expected project directory was replaced by a regular file.
    unexpected_project_file: bool,
}

struct FootprintKeys {
    primary: (String, ProfileFamilyV1),
    overlay: Option<(String, ProfileFamilyV1)>,
    /// For a file in a project store: the project id and the store-relative path.
    project: Option<(String, PathBuf)>,
}

/// Charge the profile census to the same entry budget as its caller's other
/// filesystem observations.
pub(crate) fn census_profile_footprint_with_budget(
    profile_root: &Path,
    registered_roots: &BTreeMap<String, String>,
    bound_overrides: &BTreeMap<String, u64>,
    remaining_entries: &mut usize,
) -> ProfileFootprintV1 {
    let mut tallies: BTreeMap<(String, ProfileFamilyV1), EntryTally> = BTreeMap::new();
    let mut total_bytes = 0u64;
    let mut unavailable_entry_count = 0usize;
    let termination = walk_regular_files_with_budget(
        profile_root,
        remaining_entries,
        &|| None,
        &mut |path, walked| {
            if let WalkedEntryV1::File { bytes } = walked {
                total_bytes = total_bytes.saturating_add(bytes);
            } else {
                unavailable_entry_count = unavailable_entry_count.saturating_add(1);
            }
            let Some(keys) = path
                .strip_prefix(profile_root)
                .ok()
                .and_then(footprint_keys)
            else {
                return;
            };
            // The file itself is measurable, but it cannot be a usable
            // project-store directory. Keep its bytes in the physical total,
            // and report the missing store traversal as unavailable rather
            // than calling the replaced project healthy.
            let unexpected_project_file = matches!(walked, WalkedEntryV1::File { .. })
                && keys
                    .project
                    .as_ref()
                    .is_some_and(|(_, relative)| relative.as_os_str().is_empty());
            if unexpected_project_file {
                unavailable_entry_count = unavailable_entry_count.saturating_add(1);
            }
            let primary = tallies.entry(keys.primary).or_default();
            tally(primary, walked);
            if unexpected_project_file {
                primary.unavailable_entry_count = primary.unavailable_entry_count.saturating_add(1);
                primary.unexpected_project_file = true;
            }
            if let (Some((_, store_relative)), WalkedEntryV1::File { bytes }) =
                (&keys.project, walked)
                && !unexpected_project_file
            {
                primary
                    .kinds
                    .get_or_insert_with(StoreKindBytesV1::default)
                    .add(store_relative, bytes);
            }
            if let Some(overlay) = keys.overlay {
                tally(tallies.entry(overlay).or_default(), walked);
            }
        },
    );
    let entries = tallies
        .into_iter()
        .map(|((key, family), tally)| {
            let project = (family == ProfileFamilyV1::ProjectStore).then(|| {
                let project_id = key.strip_prefix("projects/").unwrap_or(&key);
                let registered_root = registered_roots.get(project_id).cloned();
                ProjectStoreRootV1 {
                    state: ProjectRootStateV1::probe(registered_root.as_deref()),
                    registered_root,
                }
            });
            ProfileFootprintEntryV1 {
                bound_bytes: bound_overrides.get(&key).copied().unwrap_or_else(|| {
                    if tally.unexpected_project_file {
                        // An unrelated file must not inherit a project's 10 GiB
                        // allowance. An explicit owner override still wins.
                        default_bound_bytes(ProfileFamilyV1::Other)
                    } else {
                        default_bound_bytes(family)
                    }
                }),
                key,
                family,
                bytes: tally.bytes,
                unavailable_entry_count: tally.unavailable_entry_count,
                project,
                kinds: tally.kinds,
            }
        })
        .collect();
    ProfileFootprintV1 {
        entries,
        total_bytes,
        unavailable_entry_count,
        truncated: termination == WalkTerminationV1::EntryBudgetExceeded,
        total_bound_bytes: bound_overrides
            .get(PROFILE_TOTAL_KEY)
            .copied()
            .unwrap_or(PROFILE_TOTAL_BOUND_BYTES),
    }
}

/// Footprint key of a process's unlinked `SQLite` temp files.
pub const SQLITE_TEMP_SPILL_KEY: &str = "sqlite_temp_spill";

/// Size the unlinked `SQLite` temp files (`etilqs_*` TEMP databases and sort
/// spills) a process holds open, read through its fd directory, normally
/// `/proc/self/fd`. They live outside the profile, so the profile walk never
/// sees them (#4152). `None` when the directory cannot be read, as off Linux.
#[must_use]
pub fn census_sqlite_temp_spill(
    fd_directory: &Path,
    bound_overrides: &BTreeMap<String, u64>,
) -> Option<ProfileFootprintEntryV1> {
    let mut bytes = 0u64;
    let mut unavailable_entry_count = 0usize;
    // Multiple descriptors may refer to one unlinked SQLite allocation.
    #[cfg(unix)]
    let mut observed_files = BTreeSet::new();
    for entry in std::fs::read_dir(fd_directory).ok()? {
        let fd = match entry {
            Ok(fd) => fd,
            Err(_) => {
                unavailable_entry_count = unavailable_entry_count.saturating_add(1);
                continue;
            }
        };
        let target = match std::fs::read_link(fd.path()) {
            Ok(target) => target,
            // Descriptors can close between enumeration and observation.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                unavailable_entry_count = unavailable_entry_count.saturating_add(1);
                continue;
            }
        };
        let unlinked_temp = target
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("etilqs_") && name.ends_with(" (deleted)"));
        if unlinked_temp {
            match std::fs::metadata(fd.path()) {
                Ok(metadata) => {
                    #[cfg(unix)]
                    if !observed_files.insert((metadata.dev(), metadata.ino())) {
                        continue;
                    }
                    bytes = bytes.saturating_add(metadata.len());
                }
                // A closed descriptor no longer consumes space.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => unavailable_entry_count = unavailable_entry_count.saturating_add(1),
            }
        }
    }
    Some(ProfileFootprintEntryV1 {
        key: SQLITE_TEMP_SPILL_KEY.to_owned(),
        family: ProfileFamilyV1::SqliteTempSpill,
        bytes,
        unavailable_entry_count,
        bound_bytes: bound_overrides
            .get(SQLITE_TEMP_SPILL_KEY)
            .copied()
            .unwrap_or_else(|| default_bound_bytes(ProfileFamilyV1::SqliteTempSpill)),
        project: None,
        kinds: None,
    })
}

fn tally(entry: &mut EntryTally, walked: WalkedEntryV1) {
    match walked {
        WalkedEntryV1::File { bytes } => entry.bytes = entry.bytes.saturating_add(bytes),
        WalkedEntryV1::Unavailable => {
            entry.unavailable_entry_count = entry.unavailable_entry_count.saturating_add(1);
        }
    }
}

fn is_wal(name: &str) -> bool {
    name.ends_with("-wal") || name.ends_with(".wal")
}

// SQLite WALs are regular root-level files. Only Grafeo WALs are
// directories, so a similarly named incident tree must not inherit a WAL
// budget or hide its own independently bounded footprint.
fn is_wal_family_root(name: &str, is_file: bool) -> bool {
    is_wal(name) && (is_file || name.ends_with(".grafeo.wal"))
}

/// The footprint keys one profile-relative path belongs to. A WAL directory
/// is one entry keyed by the directory; its segments sum into it.
fn footprint_keys(relative: &Path) -> Option<FootprintKeys> {
    let components = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let first = components.first()?;
    // A non-traversable project root (symlink, unreadable directory, or
    // unexpected file) still belongs to that project's named finding.
    // Otherwise its unavailable bytes appear under a generic `projects` key
    // while the affected store disappears from Doctor altogether.
    if first == PROJECTS_DIRECTORY && components.len() >= 2 {
        let store_key = format!("{PROJECTS_DIRECTORY}/{}", components[1]);
        let inner = &components[2..];
        // A container's verified/spill tree owns its descendants, even when
        // a scratch file is named *.wal. Only the actual container WAL
        // directory (or a WAL outside a container) gets a WAL bound.
        let overlay = inner
            .split_first()
            .and_then(|(first, rest)| {
                graph_container_stem(first, rest.is_empty()).map(|stem| {
                    (
                        format!("{store_key}/{stem}"),
                        ProfileFamilyV1::GraphContainer,
                    )
                })
            })
            .or_else(|| {
                inner
                    .first()
                    .filter(|name| is_wal_family_root(name, inner.len() == 1))
                    .map(|first| (format!("{store_key}/{first}"), ProfileFamilyV1::Wal))
            });
        return Some(FootprintKeys {
            primary: (store_key, ProfileFamilyV1::ProjectStore),
            overlay,
            project: Some((components[1].clone(), inner.iter().collect())),
        });
    }
    // A top-level family owns all descendants. WAL-looking nested scratch
    // filenames in an otherwise unrelated tree must not escape that bound.
    let primary = if let Some(stem) = graph_container_stem(first, components.len() == 1) {
        // Sidecar trees take precedence over WAL-looking scratch filenames.
        (stem.to_owned(), ProfileFamilyV1::GraphContainer)
    } else if is_wal_family_root(first, components.len() == 1) {
        (first.clone(), ProfileFamilyV1::Wal)
    } else if components.len() == 1
        && matches!(
            first.as_str(),
            "global.db" | "global.db-shm" | "global.db-journal"
        )
    {
        ("global.db".to_owned(), ProfileFamilyV1::ProfileRegistry)
    } else if components.len() == 1
        && matches!(
            first.as_str(),
            "user-sessions.db" | "user-sessions.db-shm" | "user-sessions.db-journal"
        )
    {
        ("user-sessions.db".to_owned(), ProfileFamilyV1::UserSessions)
    } else if components.len() == 1
        && matches!(
            first.as_str(),
            "user-memory.db" | "user-memory.db-shm" | "user-memory.db-journal"
        )
    {
        ("user-memory".to_owned(), ProfileFamilyV1::UserMemory)
    } else {
        let family = match first.as_str() {
            "lcm-payloads" => ProfileFamilyV1::LcmPayloads,
            "hook_analytics.jsonl" => ProfileFamilyV1::HookAnalytics,
            "response-handles" => ProfileFamilyV1::ResponseHandles,
            "maintenance" => ProfileFamilyV1::Maintenance,
            _ => ProfileFamilyV1::Other,
        };
        (first.clone(), family)
    };
    Some(FootprintKeys {
        primary,
        overlay: None,
        project: None,
    })
}

/// Keep a stable evidence reference even for deeply nested filesystem names.
/// The report retains the complete key; Doctor references have a smaller bound.
fn doctor_footprint_reference_key(key: &str) -> Cow<'_, str> {
    if DoctorEvidenceReferenceV1::new(key.to_owned()).is_ok() {
        return Cow::Borrowed(key);
    }
    Cow::Owned(format!("footprint/sha256:{}", sha256_hex(key.as_bytes())))
}

/// Preserve the measured size and bound in Doctor even when an optional
/// registered root, kind breakdown, or long footprint identity exceeds its
/// 512-byte coverage statement. Full identities and breakdowns remain in the
/// storage report; a truncated number would make the finding unactionable.
fn bounded_entry_subject(entry: &ProfileFootprintEntryV1) -> (String, String) {
    let mut evidence_key = doctor_footprint_reference_key(&entry.key).into_owned();
    let mut subject = entry_statement(entry, &evidence_key);
    let fits = |subject: &str| {
        let statement = format!(
            "{subject}: {} of {} bound",
            format_bytes(entry.bytes),
            format_bytes(entry.bound_bytes)
        );
        super::diagnostics::bounded_statement(&statement) == statement
    };
    if !fits(&subject) {
        let mut compact = entry.clone();
        compact.kinds = None;
        if let Some(project) = compact.project.as_mut() {
            // Keep the verified root state, but omit its optional long path.
            project.registered_root = None;
        }
        subject = entry_statement(&compact, &evidence_key);
        if !fits(&subject) {
            // A valid Doctor reference can be longer than its statement budget.
            // Retain stable evidence identity without losing the numeric bound.
            evidence_key = format!("footprint/sha256:{}", sha256_hex(entry.key.as_bytes()));
            subject = entry_statement(&compact, &evidence_key);
        }
    }
    (evidence_key, subject)
}

/// One `OverBudgetStore` finding per footprint entry plus one for the profile
/// total. A floor above its bound proves the overage, so an over-bound entry
/// is degraded with complete coverage even when bytes went unread.
#[must_use]
pub fn footprint_findings(footprint: &ProfileFootprintV1) -> DoctorStorageFamilyReadV1 {
    let mut findings = Vec::with_capacity(footprint.entries.len() + 1);
    for entry in &footprint.entries {
        let (evidence_key, subject) = bounded_entry_subject(entry);
        let Some(finding) = bounded_finding(
            evidence_key.as_str(),
            &subject,
            entry.bytes,
            entry.bound_bytes,
            entry.unavailable_entry_count > 0 || footprint.truncated,
        ) else {
            return DoctorStorageFamilyReadV1::Unknown;
        };
        findings.push(finding);
    }
    let total_statement = format!(
        "profile total{}",
        floor_note(footprint.unavailable_entry_count, footprint.truncated)
    );
    let Some(total) = bounded_finding(
        PROFILE_TOTAL_KEY,
        &total_statement,
        footprint.total_bytes,
        footprint.total_bound_bytes,
        footprint.unavailable_entry_count > 0 || footprint.truncated,
    ) else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    findings.push(total);
    storage_family_read(findings)
}

fn floor_note(unavailable_entry_count: usize, truncated: bool) -> String {
    match (unavailable_entry_count, truncated) {
        (0, false) => String::new(),
        (0, true) => " (floor: walk stopped at its entry ceiling)".to_owned(),
        (count, false) => format!(" (floor: {count} unreadable entries)"),
        (count, true) => {
            format!(" (floor: {count} unreadable entries, walk stopped at its entry ceiling)")
        }
    }
}

fn entry_statement(entry: &ProfileFootprintEntryV1, evidence_key: &str) -> String {
    let label = match entry.family {
        ProfileFamilyV1::ProjectStore => "project store",
        ProfileFamilyV1::GraphContainer => "graph container",
        ProfileFamilyV1::Wal => "WAL",
        ProfileFamilyV1::ProfileRegistry => "profile registry",
        ProfileFamilyV1::UserSessions => "user sessions",
        ProfileFamilyV1::UserMemory => "user memory",
        ProfileFamilyV1::LcmPayloads => "LCM payloads",
        ProfileFamilyV1::HookAnalytics => "hook analytics",
        ProfileFamilyV1::ResponseHandles => "response handles",
        ProfileFamilyV1::Maintenance => "maintenance state",
        ProfileFamilyV1::SqliteTempSpill => "unlinked SQLite temp files",
        ProfileFamilyV1::Other => "profile entry",
    };
    let floor = floor_note(entry.unavailable_entry_count, false);
    if evidence_key != entry.key {
        return format!("{label} {evidence_key} (full path in storage report){floor}");
    }
    let root =
        entry
            .project
            .as_ref()
            .map_or_else(String::new, |project| match &project.registered_root {
                Some(root) => format!(" for {root} ({})", project.state.label()),
                None => format!(" ({})", project.state.label()),
            });
    let breakdown = entry
        .kinds
        .filter(|kinds| kinds.graph_container > 0 || kinds.graph_replay_pool > 0)
        .map_or_else(String::new, |kinds| {
            format!(
                "; graph container {}, replay pool {}",
                format_bytes(kinds.graph_container),
                format_bytes(kinds.graph_replay_pool)
            )
        });
    format!("{label} {evidence_key}{root}{breakdown}{floor}")
}

fn bounded_finding(
    key: &str,
    subject: &str,
    bytes: u64,
    bound_bytes: u64,
    floor: bool,
) -> Option<DoctorStorageFindingV1> {
    let (state, completeness) = if bytes > bound_bytes {
        (
            DoctorEvidenceStateV1::Degraded,
            DoctorCoverageCompletenessV1::Complete,
        )
    } else if floor {
        (
            DoctorEvidenceStateV1::Partial,
            DoctorCoverageCompletenessV1::Partial,
        )
    } else {
        (
            DoctorEvidenceStateV1::HealthyCompleteCoverage,
            DoctorCoverageCompletenessV1::Complete,
        )
    };
    let statement = format!(
        "{subject}: {} of {} bound",
        format_bytes(bytes),
        format_bytes(bound_bytes)
    );
    let reference = DoctorEvidenceReferenceV1::new(key.to_owned()).ok()?;
    let evidence = DoctorEvidenceRefV1::new(DoctorFindingFamilyV1::Storage, reference);
    let coverage = DoctorCoverageStatementV1::new(
        completeness,
        super::diagnostics::bounded_statement(&statement),
    )
    .ok()?;
    let core = DoctorFindingV1::new(
        DoctorFindingFamilyV1::Storage,
        state,
        vec![evidence],
        coverage,
    )
    .ok()?;
    DoctorStorageFindingV1::new(DoctorStorageFindingKindV1::OverBudgetStore, core).ok()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use tracedecay_contracts::doctor::{
        DoctorEvidenceStateV1, DoctorStorageFamilyReadV1, DoctorStorageFindingV1,
    };

    use super::*;

    const LIVE_DB_BYTES: u64 = 3_000_000;
    const WAL_BYTES: u64 = 129 * MIB;

    fn sized(path: &Path, bytes: u64) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("parent directory");
        std::fs::File::create(path)
            .expect("fixture file")
            .set_len(bytes)
            .expect("fixture length");
    }

    fn findings(footprint: &ProfileFootprintV1) -> BTreeMap<String, DoctorStorageFindingV1> {
        let DoctorStorageFamilyReadV1::Observed { findings } = footprint_findings(footprint) else {
            panic!("a footprint always observes its entries");
        };
        findings
            .into_iter()
            .map(|finding| {
                let key = finding.finding().evidence()[0]
                    .reference()
                    .as_str()
                    .to_owned();
                (key, finding)
            })
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn temp_spill_census_sums_only_unlinked_sqlite_temp_files() {
        let temporary = tempfile::TempDir::new().expect("temporary directory");
        let fd_directory = temporary.path().join("fd");
        std::fs::create_dir(&fd_directory).expect("fd directory");
        for (fd, name, bytes) in [
            ("3", "etilqs_5e598e639aa4c638 (deleted)", 1_900 * MIB),
            ("4", "etilqs_8564432688f1581f (deleted)", 24 * MIB),
            ("5", "etilqs_c7305d0bad417a83", 7 * MIB),
            ("6", "sessions.db (deleted)", 11 * MIB),
        ] {
            let target = temporary.path().join(name);
            sized(&target, bytes);
            std::os::unix::fs::symlink(&target, fd_directory.join(fd)).expect("fd link");
        }

        // Duplicated file descriptors must not double-count a physical inode.
        std::os::unix::fs::symlink(
            temporary.path().join("etilqs_5e598e639aa4c638 (deleted)"),
            fd_directory.join("7"),
        )
        .expect("duplicated temp descriptor");

        let entry = census_sqlite_temp_spill(&fd_directory, &BTreeMap::new())
            .expect("a readable fd directory");

        assert_eq!(entry.key, SQLITE_TEMP_SPILL_KEY);
        assert_eq!(entry.bytes, 1_924 * MIB);
        assert_eq!(entry.bound_bytes, GIB);
        let healthy_budget = BTreeMap::from([(SQLITE_TEMP_SPILL_KEY.to_owned(), 2 * GIB)]);
        let healthy = census_sqlite_temp_spill(&fd_directory, &healthy_budget)
            .expect("same unlinked SQLite allocations");
        assert_eq!(healthy.bytes, 1_924 * MIB);
        assert_eq!(healthy.bound_bytes, 2 * GIB);
        let healthy_footprint = ProfileFootprintV1 {
            entries: vec![healthy],
            total_bytes: 0,
            unavailable_entry_count: 0,
            truncated: false,
            total_bound_bytes: PROFILE_TOTAL_BOUND_BYTES,
        };
        assert_eq!(
            findings(&healthy_footprint)[SQLITE_TEMP_SPILL_KEY]
                .finding()
                .state(),
            DoctorEvidenceStateV1::HealthyCompleteCoverage,
            "duplicate handles must not cause a false storage-pressure finding"
        );
        let footprint = ProfileFootprintV1 {
            entries: vec![entry],
            total_bytes: 0,
            unavailable_entry_count: 0,
            truncated: false,
            total_bound_bytes: PROFILE_TOTAL_BOUND_BYTES,
        };
        assert_eq!(
            findings(&footprint)[SQLITE_TEMP_SPILL_KEY]
                .finding()
                .state(),
            DoctorEvidenceStateV1::Degraded,
        );
        assert!(
            census_sqlite_temp_spill(&temporary.path().join("absent"), &BTreeMap::new()).is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn temp_spill_census_reports_unreadable_fd_entries_without_fabricating_health() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let fd_directory = temporary.path().join("fd");
        std::fs::create_dir(&fd_directory).expect("fd directory");
        let live_temp = temporary.path().join("etilqs_active (deleted)");
        sized(&live_temp, 24 * MIB);
        std::os::unix::fs::symlink(&live_temp, fd_directory.join("3")).expect("fd link");
        // A real /proc/self/fd directory contains descriptor symlinks. A
        // regular entry models an FD whose target cannot be inspected.
        std::fs::write(fd_directory.join("4"), b"uninspectable").expect("unreadable fd");
        // A descriptor closed after enumeration is not still consuming space.
        std::os::unix::fs::symlink(
            temporary.path().join("etilqs_closed (deleted)"),
            fd_directory.join("5"),
        )
        .expect("closed fd link");

        let entry = census_sqlite_temp_spill(&fd_directory, &BTreeMap::new())
            .expect("readable fd directory");
        assert_eq!(entry.bytes, 24 * MIB);
        assert_eq!(entry.unavailable_entry_count, 1);
        let footprint = ProfileFootprintV1 {
            entries: vec![entry],
            total_bytes: 0,
            unavailable_entry_count: 0,
            truncated: false,
            total_bound_bytes: PROFILE_TOTAL_BOUND_BYTES,
        };
        let observed = findings(&footprint);
        let finding = &observed[SQLITE_TEMP_SPILL_KEY];
        assert_eq!(finding.finding().state(), DoctorEvidenceStateV1::Partial);
        assert!(
            finding
                .finding()
                .coverage()
                .statement()
                .contains("1 unreadable entries")
        );

        // A measured lower bound above the configured cap still proves overage.
        let overrides = BTreeMap::from([(SQLITE_TEMP_SPILL_KEY.to_owned(), MIB)]);
        let over = census_sqlite_temp_spill(&fd_directory, &overrides).expect("over-bound census");
        assert_eq!(over.unavailable_entry_count, 1);
        assert_eq!(
            findings(&ProfileFootprintV1 {
                entries: vec![over],
                total_bytes: 0,
                unavailable_entry_count: 0,
                truncated: false,
                total_bound_bytes: PROFILE_TOTAL_BOUND_BYTES,
            })[SQLITE_TEMP_SPILL_KEY]
                .finding()
                .state(),
            DoctorEvidenceStateV1::Degraded
        );
    }

    fn entry<'a>(footprint: &'a ProfileFootprintV1, key: &str) -> &'a ProfileFootprintEntryV1 {
        footprint
            .entries
            .iter()
            .find(|entry| entry.key == key)
            .unwrap_or_else(|| panic!("footprint entry {key}"))
    }

    struct Fixture {
        _temporary: tempfile::TempDir,
        profile: std::path::PathBuf,
        registered: BTreeMap<String, String>,
        live_root: String,
        gone_root: String,
    }

    fn profile_fixture() -> Fixture {
        let temporary = tempfile::tempdir().expect("profile fixture");
        let profile = temporary.path().join("profile");
        let live_root = temporary.path().join("live-checkout");
        std::fs::create_dir_all(&live_root).expect("live root");
        let gone_root = temporary.path().join("removed-checkout");
        for (relative, bytes) in [
            ("projects/proj_live/tracedecay.db", LIVE_DB_BYTES),
            ("projects/proj_live/tracedecay.db-wal", WAL_BYTES),
            ("projects/proj_live/tracedecay.grafeo", 5_000),
            (
                "projects/proj_live/tracedecay.grafeo.wal/wal_00000001.log",
                700,
            ),
            (
                "projects/proj_live/tracedecay.grafeo.wal/wal_00000002.log",
                300,
            ),
            (
                "projects/proj_live/tracedecay.graph-replay/generation.grafeo",
                400,
            ),
            ("projects/proj_gone/tracedecay.db", 2_000),
            ("projects/proj_stray/tracedecay.db", 1_000),
            ("lcm-payloads/a", 11),
            ("lcm-payloads/b", 22),
            ("hook_analytics.jsonl", 33),
            ("response-handles/x", 44),
            ("maintenance/c.json", 55),
            ("global.db", 66),
            ("global.db-wal", 77),
            ("user-sessions.grafeo", 88),
            ("user-sessions.grafeo.wal/wal_00000001.log", 99),
            ("stranger/blob", 110),
        ] {
            sized(&profile.join(relative), bytes);
        }
        let live_root = live_root.to_string_lossy().into_owned();
        let gone_root = gone_root.to_string_lossy().into_owned();
        let registered = BTreeMap::from([
            ("proj_live".to_owned(), live_root.clone()),
            ("proj_gone".to_owned(), gone_root.clone()),
        ]);
        Fixture {
            _temporary: temporary,
            profile,
            registered,
            live_root,
            gone_root,
        }
    }

    #[test]
    fn census_sizes_every_family_and_flags_only_the_blocked_wal() {
        let fixture = profile_fixture();
        let footprint =
            census_profile_footprint(&fixture.profile, &fixture.registered, &BTreeMap::new());

        let expected = [
            (
                "projects/proj_live",
                ProfileFamilyV1::ProjectStore,
                LIVE_DB_BYTES + WAL_BYTES + 5_000 + 1_000 + 400,
            ),
            (
                "projects/proj_live/tracedecay.db-wal",
                ProfileFamilyV1::Wal,
                WAL_BYTES,
            ),
            (
                "projects/proj_live/tracedecay.grafeo",
                ProfileFamilyV1::GraphContainer,
                5_000,
            ),
            (
                "projects/proj_live/tracedecay.grafeo.wal",
                ProfileFamilyV1::Wal,
                1_000,
            ),
            ("projects/proj_gone", ProfileFamilyV1::ProjectStore, 2_000),
            ("projects/proj_stray", ProfileFamilyV1::ProjectStore, 1_000),
            ("lcm-payloads", ProfileFamilyV1::LcmPayloads, 33),
            ("hook_analytics.jsonl", ProfileFamilyV1::HookAnalytics, 33),
            ("response-handles", ProfileFamilyV1::ResponseHandles, 44),
            ("maintenance", ProfileFamilyV1::Maintenance, 55),
            ("global.db", ProfileFamilyV1::ProfileRegistry, 66),
            ("global.db-wal", ProfileFamilyV1::Wal, 77),
            ("user-sessions.grafeo", ProfileFamilyV1::GraphContainer, 88),
            ("user-sessions.grafeo.wal", ProfileFamilyV1::Wal, 99),
            ("stranger", ProfileFamilyV1::Other, 110),
        ];
        let observed = footprint
            .entries
            .iter()
            .map(|entry| (entry.key.as_str(), entry.family, entry.bytes))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            observed,
            expected.into_iter().collect(),
            "every file lands in exactly the expected keys"
        );
        assert_eq!(
            footprint.total_bytes,
            LIVE_DB_BYTES
                + WAL_BYTES
                + 5_000
                + 1_000
                + 400
                + 2_000
                + 1_000
                + 33
                + 33
                + 44
                + 55
                + 66
                + 77
                + 88
                + 99
                + 110,
            "the profile total sums walked files, not overlapping entries"
        );
        assert_eq!(
            footprint.full_profile_size().state,
            ProfileTotalCoverageStateV1::Complete
        );

        let live = entry(&footprint, "projects/proj_live");
        assert_eq!(
            live.project,
            Some(ProjectStoreRootV1 {
                registered_root: Some(fixture.live_root.clone()),
                state: ProjectRootStateV1::Present,
            })
        );
        let kinds = live.kinds.expect("project store kinds");
        assert_eq!(kinds.graph_container, 6_000);
        assert_eq!(kinds.graph_replay_pool, 400);
        assert_eq!(
            entry(&footprint, "projects/proj_gone").project,
            Some(ProjectStoreRootV1 {
                registered_root: Some(fixture.gone_root.clone()),
                state: ProjectRootStateV1::Missing,
            })
        );
        assert_eq!(
            entry(&footprint, "projects/proj_stray").project,
            Some(ProjectStoreRootV1 {
                registered_root: None,
                state: ProjectRootStateV1::Unregistered,
            })
        );

        let findings = findings(&footprint);
        assert_eq!(findings.len(), footprint.entries.len() + 1);
        for (key, finding) in &findings {
            let expected = if key == "projects/proj_live/tracedecay.db-wal" {
                DoctorEvidenceStateV1::Degraded
            } else {
                DoctorEvidenceStateV1::HealthyCompleteCoverage
            };
            assert_eq!(finding.finding().state(), expected, "{key}");
        }
        assert_eq!(
            findings["projects/proj_gone"]
                .finding()
                .coverage()
                .statement(),
            format!(
                "project store projects/proj_gone for {} (root missing on disk): 2.0 KB of 10.0 GB bound",
                fixture.gone_root
            )
        );
        assert!(
            findings["projects/proj_live"]
                .finding()
                .coverage()
                .statement()
                .contains("graph container 5.9 KB, replay pool 400 B")
        );
    }

    #[test]
    fn an_owner_override_replaces_the_default_bound() {
        let fixture = profile_fixture();
        let overrides = BTreeMap::from([("lcm-payloads".to_owned(), 32)]);
        let footprint = census_profile_footprint(&fixture.profile, &fixture.registered, &overrides);

        assert_eq!(entry(&footprint, "lcm-payloads").bound_bytes, 32);
        assert_eq!(
            findings(&footprint)["lcm-payloads"].finding().state(),
            DoctorEvidenceStateV1::Degraded
        );
    }

    #[test]
    fn a_walk_stopped_at_its_entry_ceiling_reports_a_floor() {
        let fixture = profile_fixture();
        let mut remaining_entries = 3;
        let footprint = census_profile_footprint_with_budget(
            &fixture.profile,
            &fixture.registered,
            &BTreeMap::new(),
            &mut remaining_entries,
        );

        assert!(footprint.truncated);
        assert_eq!(remaining_entries, 0);
        assert_eq!(
            footprint.full_profile_size().state,
            ProfileTotalCoverageStateV1::Partial
        );
        let findings = findings(&footprint);
        assert_eq!(
            findings["profile"].finding().state(),
            DoctorEvidenceStateV1::Partial,
            "a within-bound floor cannot claim health"
        );
    }

    #[cfg(unix)]
    #[test]
    fn controlled_walk_counts_zero_byte_entries_and_never_follows_symlink_trees() {
        let temporary = tempfile::tempdir().expect("walk fixture");
        let root = temporary.path().join("store");
        let outside = temporary.path().join("outside");
        std::fs::create_dir_all(&root).expect("store root");
        std::fs::create_dir_all(&outside).expect("outside root");
        std::fs::write(root.join("zero"), []).expect("zero-byte entry");
        sized(&outside.join("private"), 900);
        std::os::unix::fs::symlink(&outside, root.join("other-store")).expect("store symlink");
        let mut deep = root.clone();
        for _ in 0..64 {
            deep.push("d");
            std::fs::create_dir(&deep).expect("deep directory");
        }
        sized(&deep.join("payload"), 12);

        let mut remaining = 100;
        let mut bytes = 0u64;
        let mut unavailable = 0usize;
        let termination =
            walk_regular_files_with_budget(&root, &mut remaining, &|| None, &mut |_, entry| {
                match entry {
                    WalkedEntryV1::File { bytes: size } => bytes += size,
                    WalkedEntryV1::Unavailable => unavailable += 1,
                }
            });

        assert_eq!(termination, WalkTerminationV1::Complete);
        assert_eq!(bytes, 12, "the outside store is not followed");
        assert_eq!(unavailable, 1, "the symlink is explicit unavailable data");
        assert_eq!(remaining, 100 - 1 - 64 - 1 - 1);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_project_root_has_its_own_partial_finding() {
        let temporary = tempfile::tempdir().expect("footprint fixture");
        let profile = temporary.path().join("profile");
        let foreign = temporary.path().join("foreign");
        sized(&foreign.join("private"), 2 * GIB);
        sized(&profile.join("projects/proj_live/tracedecay.db"), 11);
        std::os::unix::fs::symlink(&foreign, profile.join("projects/proj_blocked"))
            .expect("substituted project directory");

        let registered_root = temporary.path().join("registered-checkout");
        std::fs::create_dir(&registered_root).expect("registered root");
        let registered = BTreeMap::from([(
            "proj_blocked".to_owned(),
            registered_root.display().to_string(),
        )]);
        let overrides = BTreeMap::from([
            ("projects/proj_blocked".to_owned(), 1),
            ("projects/proj_live".to_owned(), 11),
            (PROFILE_TOTAL_KEY.to_owned(), 11),
        ]);

        let footprint = census_profile_footprint(&profile, &registered, &overrides);
        assert_eq!(
            footprint.total_bytes, 11,
            "foreign bytes must not be counted"
        );
        assert_eq!(footprint.unavailable_entry_count, 1);
        assert_eq!(footprint.entries.len(), 2, "no generic projects entry");
        assert_eq!(
            footprint.full_profile_size().state,
            ProfileTotalCoverageStateV1::Partial
        );
        let blocked = entry(&footprint, "projects/proj_blocked");
        assert_eq!(blocked.family, ProfileFamilyV1::ProjectStore);
        assert_eq!(blocked.bytes, 0);
        assert_eq!(blocked.unavailable_entry_count, 1);
        assert_eq!(blocked.bound_bytes, 1);
        assert_eq!(
            blocked.project.as_ref().expect("registered project").state,
            ProjectRootStateV1::Present
        );
        assert_eq!(entry(&footprint, "projects/proj_live").bytes, 11);

        let observed = findings(&footprint);
        assert_eq!(observed.len(), 3);
        assert_eq!(
            observed["projects/proj_blocked"].finding().state(),
            DoctorEvidenceStateV1::Partial,
            "an untraversable project store cannot be reported healthy or omitted"
        );
        assert_eq!(
            observed["projects/proj_live"].finding().state(),
            DoctorEvidenceStateV1::HealthyCompleteCoverage
        );
        assert_eq!(
            observed[PROFILE_TOTAL_KEY].finding().state(),
            DoctorEvidenceStateV1::Partial
        );
    }

    #[test]
    fn regular_file_replacing_project_directory_is_not_healthy_or_project_budgeted() {
        let temporary = tempfile::tempdir().expect("footprint fixture");
        let profile = temporary.path().join("profile");
        sized(&profile.join("projects/proj_replaced"), 11);
        sized(&profile.join("projects/proj_healthy/tracedecay.db"), 7);
        let registered_root = temporary.path().join("registered-checkout");
        std::fs::create_dir(&registered_root).expect("registered root");
        let registered = BTreeMap::from([(
            "proj_replaced".to_owned(),
            registered_root.display().to_string(),
        )]);

        let footprint = census_profile_footprint(&profile, &registered, &BTreeMap::new());
        assert_eq!(footprint.total_bytes, 18, "count all regular-file bytes");
        assert_eq!(footprint.unavailable_entry_count, 1);
        assert_eq!(footprint.entries.len(), 2, "name the replaced store");
        let replaced = entry(&footprint, "projects/proj_replaced");
        assert_eq!(replaced.family, ProfileFamilyV1::ProjectStore);
        assert_eq!(replaced.bytes, 11, "do not hide the replacement file");
        assert_eq!(replaced.unavailable_entry_count, 1);
        assert_eq!(
            replaced.bound_bytes,
            default_bound_bytes(ProfileFamilyV1::Other),
            "a replacement file must not inherit the 10 GiB project allowance"
        );
        assert!(replaced.kinds.is_none(), "not a valid project-store file");
        assert_eq!(
            replaced.project.as_ref().expect("registration").state,
            ProjectRootStateV1::Present
        );
        let healthy = entry(&footprint, "projects/proj_healthy");
        assert_eq!(healthy.bytes, 7);
        assert_eq!(healthy.unavailable_entry_count, 0);
        assert_eq!(
            healthy.bound_bytes,
            default_bound_bytes(ProfileFamilyV1::ProjectStore)
        );

        let observed = findings(&footprint);
        assert_eq!(observed.len(), 3);
        assert_eq!(
            observed["projects/proj_replaced"].finding().state(),
            DoctorEvidenceStateV1::Partial
        );
        assert_eq!(
            observed["projects/proj_healthy"].finding().state(),
            DoctorEvidenceStateV1::HealthyCompleteCoverage
        );
        assert_eq!(
            observed[PROFILE_TOTAL_KEY].finding().state(),
            DoctorEvidenceStateV1::Partial
        );

        // Exercise the same overage path without allocating a 1 GiB fixture
        // on platforms that eagerly provision extended regular files.
        let overrides = BTreeMap::from([("projects/proj_replaced".to_owned(), 10)]);
        let over = census_profile_footprint(&profile, &registered, &overrides);
        assert_eq!(over.total_bytes, 18);
        assert_eq!(entry(&over, "projects/proj_replaced").bound_bytes, 10);
        assert_eq!(
            findings(&over)["projects/proj_replaced"].finding().state(),
            DoctorEvidenceStateV1::Degraded,
            "a large replacement file must trigger its tighter named bound"
        );
    }

    #[test]
    fn one_entry_budget_is_shared_across_walk_roots() {
        let temporary = tempfile::tempdir().expect("walk fixture");
        let first = temporary.path().join("first");
        let second = temporary.path().join("second");
        std::fs::create_dir_all(&first).expect("first root");
        std::fs::create_dir_all(&second).expect("second root");
        std::fs::write(first.join("payload"), [1]).expect("first payload");
        std::fs::write(second.join("payload"), [2]).expect("second payload");
        let mut remaining = 1;
        let mut visits = 0usize;
        let mut visit = |_: &Path, _: WalkedEntryV1| visits += 1;

        assert_eq!(
            walk_regular_files_with_budget(&first, &mut remaining, &|| None, &mut visit),
            WalkTerminationV1::Complete
        );
        assert_eq!(
            walk_regular_files_with_budget(&second, &mut remaining, &|| None, &mut visit),
            WalkTerminationV1::EntryBudgetExceeded
        );
        assert_eq!(visits, 1);
        assert_eq!(remaining, 0);
    }

    #[test]
    fn cancellation_stops_a_blocking_walk_at_the_next_checkpoint() {
        use std::cell::Cell;

        let temporary = tempfile::tempdir().expect("walk fixture");
        for name in ["one", "two", "three"] {
            std::fs::write(temporary.path().join(name), [1]).expect("payload");
        }
        let visits = Cell::new(0usize);
        let mut remaining = 10;
        let termination = walk_regular_files_with_budget(
            temporary.path(),
            &mut remaining,
            &|| (visits.get() == 1).then_some(WalkInterruptionV1::Cancelled),
            &mut |_, _| visits.set(visits.get() + 1),
        );

        assert_eq!(termination, WalkTerminationV1::Cancelled);
        assert_eq!(visits.get(), 1);
        assert_eq!(remaining, 9);
    }

    #[test]
    fn hashed_wal_reference_keeps_overages_and_partial_coverage() {
        // A real WAL overlay is anchored at a store root; nested payloads do
        // not create WAL families. Construct the oversized diagnostic key
        // directly to exercise the bounded Doctor evidence contract.
        let key = format!("projects/proj_deep/{}.wal", "x".repeat(1025));
        let mut footprint = ProfileFootprintV1 {
            entries: vec![
                ProfileFootprintEntryV1 {
                    key: "projects/proj_deep".to_owned(),
                    family: ProfileFamilyV1::ProjectStore,
                    bytes: 2 * GIB,
                    unavailable_entry_count: 1,
                    bound_bytes: 1,
                    project: None,
                    kinds: None,
                },
                ProfileFootprintEntryV1 {
                    key: key.clone(),
                    family: ProfileFamilyV1::Wal,
                    bytes: 2 * GIB,
                    unavailable_entry_count: 1,
                    bound_bytes: default_bound_bytes(ProfileFamilyV1::Wal),
                    project: None,
                    kinds: None,
                },
            ],
            total_bytes: 2 * GIB,
            unavailable_entry_count: 1,
            truncated: false,
            total_bound_bytes: 1,
        };
        assert!(DoctorEvidenceReferenceV1::new(key.clone()).is_err());

        let observed = findings(&footprint);
        assert_eq!(observed.len(), footprint.entries.len() + 1);
        let reference = format!("footprint/sha256:{}", sha256_hex(key.as_bytes()));
        let wal_finding = &observed[&reference];
        assert_eq!(
            wal_finding.finding().state(),
            DoctorEvidenceStateV1::Degraded
        );
        let statement = wal_finding.finding().coverage().statement();
        assert!(
            statement.contains("full path in storage report"),
            "{statement}"
        );
        assert!(statement.contains(&format_bytes(2 * GIB)), "{statement}");
        assert!(
            statement.contains(&format_bytes(default_bound_bytes(ProfileFamilyV1::Wal))),
            "{statement}"
        );
        assert!(statement.contains("1 unreadable entries"), "{statement}");
        for reference in ["projects/proj_deep", PROFILE_TOTAL_KEY] {
            assert_eq!(
                observed[reference].finding().state(),
                DoctorEvidenceStateV1::Degraded,
                "an oversized identity must not suppress other overages"
            );
        }

        footprint.entries[1].bound_bytes = 3 * GIB;
        let observed = findings(&footprint);
        let finding = observed[&reference].finding();
        assert_eq!(finding.state(), DoctorEvidenceStateV1::Partial);
        let statement = finding.coverage().statement();
        assert!(statement.contains("WAL "), "{statement}");
        assert!(statement.contains("1 unreadable entries"), "{statement}");
    }

    #[test]
    fn top_level_container_bound_excludes_unrelated_grafeo_suffixes() {
        let temp = tempfile::tempdir().expect("footprint fixture");
        let profile = temp.path().join("profile");
        for (name, bytes) in [
            ("user-sessions.grafeo", 5),
            ("user-sessions.grafeo.verified", 7),
            ("user-sessions.grafeo.spill/scratch/inflight", 11),
            ("user-sessions.grafeo.notes", 30),
            ("other.grafeo.metrics", 40),
            ("scratch.grafeo/unrelated", 13),
        ] {
            sized(&profile.join(name), bytes);
        }
        let container_key = "user-sessions.grafeo";
        let overrides = BTreeMap::from([(container_key.to_owned(), 24)]);
        let footprint = census_profile_footprint(&profile, &BTreeMap::new(), &overrides);

        assert_eq!(footprint.total_bytes, 106);
        assert_eq!(entry(&footprint, container_key).bytes, 23);
        assert_eq!(
            entry(&footprint, container_key).family,
            ProfileFamilyV1::GraphContainer
        );
        for (name, bytes) in [
            ("user-sessions.grafeo.notes", 30),
            ("other.grafeo.metrics", 40),
            ("scratch.grafeo", 13),
        ] {
            let unrelated = entry(&footprint, name);
            assert_eq!(unrelated.family, ProfileFamilyV1::Other);
            assert_eq!(unrelated.bytes, bytes);
        }
        assert_eq!(
            footprint.full_profile_size().state,
            ProfileTotalCoverageStateV1::Complete
        );
        let observed = findings(&footprint);
        assert_eq!(
            observed[container_key].finding().state(),
            DoctorEvidenceStateV1::HealthyCompleteCoverage,
            "unowned files must not falsely trip the graph-container bound"
        );

        let over = census_profile_footprint(
            &profile,
            &BTreeMap::new(),
            &BTreeMap::from([(container_key.to_owned(), 22)]),
        );
        assert_eq!(
            findings(&over)[container_key].finding().state(),
            DoctorEvidenceStateV1::Degraded,
            "actual graph sidecar bytes must still trip their bound"
        );
    }

    #[test]
    fn project_container_bound_includes_verified_and_spill_sidecars() {
        let temp = tempfile::tempdir().expect("footprint fixture");
        let profile = temp.path().join("profile");
        let project = "projects/proj_sidecars";
        for (name, bytes) in [
            ("graph.grafeo", 5),
            ("graph.grafeo.verified", 7),
            ("graph.grafeo.spill", 11),
            ("graph.grafeo.wal/segment", 13),
            ("other.grafeo.spill/scratch/inflight", 17),
            ("not-a-graph.grafeo.tmp", 19),
        ] {
            sized(&profile.join(project).join(name), bytes);
        }
        let container_key = format!("{project}/graph.grafeo");
        let sibling_key = format!("{project}/other.grafeo");
        let wal_key = format!("{project}/graph.grafeo.wal");
        let overrides = BTreeMap::from([(container_key.clone(), 10), (sibling_key.clone(), 16)]);
        let footprint = census_profile_footprint(&profile, &BTreeMap::new(), &overrides);
        assert_eq!(footprint.total_bytes, 72);
        assert_eq!(entry(&footprint, project).bytes, 72);
        assert_eq!(
            entry(&footprint, project)
                .kinds
                .expect("physical project-kind census")
                .graph_container,
            53
        );
        assert_eq!(
            entry(&footprint, &container_key).family,
            ProfileFamilyV1::GraphContainer
        );
        assert_eq!(entry(&footprint, &container_key).bytes, 23);
        assert_eq!(entry(&footprint, &sibling_key).bytes, 17);
        assert_eq!(entry(&footprint, &wal_key).family, ProfileFamilyV1::Wal);
        assert_eq!(entry(&footprint, &wal_key).bytes, 13);
        assert_eq!(
            footprint.full_profile_size().state,
            ProfileTotalCoverageStateV1::Complete
        );

        let observed = findings(&footprint);
        assert_eq!(
            observed[&container_key].finding().state(),
            DoctorEvidenceStateV1::Degraded,
            "the former main-file-only overlay would falsely report healthy"
        );
        assert_eq!(
            observed[&sibling_key].finding().state(),
            DoctorEvidenceStateV1::Degraded,
            "an orphaned spill still consumes a named container bound"
        );
        assert_eq!(
            observed[&wal_key].finding().state(),
            DoctorEvidenceStateV1::HealthyCompleteCoverage,
            "WAL pressure remains its own bound, not double-charged to containers"
        );
    }

    #[test]
    fn nested_wal_named_graph_spill_files_remain_under_container_bounds() {
        let temp = tempfile::tempdir().expect("footprint fixture");
        let profile = temp.path().join("profile");
        for (relative, bytes) in [
            ("user-sessions.grafeo", 5),
            ("user-sessions.grafeo.spill/scratch/chunk.wal", 17),
            ("user-sessions.grafeo.spill/scratch/segment-wal/part", 19),
            ("user-sessions.grafeo.wal/segment", 7),
            ("projects/proj_spill/graph.grafeo", 3),
            (
                "projects/proj_spill/graph.grafeo.spill/scratch/chunk.wal",
                23,
            ),
            (
                "projects/proj_spill/graph.grafeo.spill/segment-wal/part",
                29,
            ),
            ("projects/proj_spill/graph.grafeo.wal/segment", 11),
        ] {
            sized(&profile.join(relative), bytes);
        }
        let overrides = BTreeMap::from([
            ("user-sessions.grafeo".to_owned(), 40),
            ("projects/proj_spill/graph.grafeo".to_owned(), 54),
            ("user-sessions.grafeo.wal".to_owned(), 7),
            ("projects/proj_spill/graph.grafeo.wal".to_owned(), 11),
        ]);
        let footprint = census_profile_footprint(&profile, &BTreeMap::new(), &overrides);
        assert_eq!(footprint.total_bytes, 114);
        assert_eq!(footprint.unavailable_entry_count, 0);
        assert_eq!(footprint.entries.len(), 5);
        assert_eq!(
            footprint.full_profile_size().state,
            ProfileTotalCoverageStateV1::Complete
        );

        for (key, family, bytes) in [
            ("user-sessions.grafeo", ProfileFamilyV1::GraphContainer, 41),
            (
                "projects/proj_spill/graph.grafeo",
                ProfileFamilyV1::GraphContainer,
                55,
            ),
            ("user-sessions.grafeo.wal", ProfileFamilyV1::Wal, 7),
            (
                "projects/proj_spill/graph.grafeo.wal",
                ProfileFamilyV1::Wal,
                11,
            ),
            ("projects/proj_spill", ProfileFamilyV1::ProjectStore, 66),
        ] {
            let measured = entry(&footprint, key);
            assert_eq!(measured.family, family, "{key}");
            assert_eq!(measured.bytes, bytes, "{key}");
        }

        let observed = findings(&footprint);
        assert_eq!(observed.len(), 6);
        for key in ["user-sessions.grafeo", "projects/proj_spill/graph.grafeo"] {
            assert_eq!(
                observed[key].finding().state(),
                DoctorEvidenceStateV1::Degraded,
                "{key} must detect spill bytes despite WAL-looking filenames"
            );
        }
        for key in [
            "user-sessions.grafeo.wal",
            "projects/proj_spill/graph.grafeo.wal",
            "projects/proj_spill",
            PROFILE_TOTAL_KEY,
        ] {
            assert_eq!(
                observed[key].finding().state(),
                DoctorEvidenceStateV1::HealthyCompleteCoverage,
                "{key} must retain its actual bound"
            );
        }
    }

    #[test]
    fn database_family_bounds_do_not_swallow_unrelated_suffix_files() {
        let temp = tempfile::tempdir().expect("footprint fixture");
        let profile = temp.path().join("profile");
        for (name, bytes) in [
            ("global.db", 5),
            ("global.db-shm", 7),
            ("global.db-journal", 11),
            ("global.db-wal", 13),
            ("global.db-mystery", 17),
            ("user-sessions.db", 19),
            ("user-sessions.db-shm", 23),
            ("user-sessions.db-journal", 29),
            ("user-sessions.db-wal", 31),
            ("user-sessions.db-backup", 37),
            ("user-memory.db", 41),
            ("user-memory.db-shm", 43),
            ("user-memory.db-journal", 47),
            ("user-memory.db-wal", 53),
            ("user-memory.db-backup", 59),
            ("user-memory.grafeo.notes", 61),
        ] {
            sized(&profile.join(name), bytes);
        }
        let overrides = BTreeMap::from([
            ("global.db".to_owned(), 23),
            ("user-sessions.db".to_owned(), 71),
            ("global.db-mystery".to_owned(), 16),
            ("user-sessions.db-backup".to_owned(), 36),
            ("user-memory".to_owned(), 132),
            ("user-memory.db-wal".to_owned(), 54),
            ("user-memory.db-backup".to_owned(), 58),
            ("user-memory.grafeo.notes".to_owned(), 60),
        ]);
        let footprint = census_profile_footprint(&profile, &BTreeMap::new(), &overrides);
        assert_eq!(footprint.total_bytes, 496);
        assert_eq!(
            footprint.full_profile_size().state,
            ProfileTotalCoverageStateV1::Complete
        );

        for (key, family, bytes) in [
            ("global.db", ProfileFamilyV1::ProfileRegistry, 23),
            ("user-sessions.db", ProfileFamilyV1::UserSessions, 71),
            ("global.db-wal", ProfileFamilyV1::Wal, 13),
            ("user-sessions.db-wal", ProfileFamilyV1::Wal, 31),
            ("global.db-mystery", ProfileFamilyV1::Other, 17),
            ("user-sessions.db-backup", ProfileFamilyV1::Other, 37),
            ("user-memory", ProfileFamilyV1::UserMemory, 131),
            ("user-memory.db-wal", ProfileFamilyV1::Wal, 53),
            ("user-memory.db-backup", ProfileFamilyV1::Other, 59),
            ("user-memory.grafeo.notes", ProfileFamilyV1::Other, 61),
        ] {
            let measured = entry(&footprint, key);
            assert_eq!(measured.family, family, "{key}");
            assert_eq!(measured.bytes, bytes, "{key}");
        }
        let observed = findings(&footprint);
        assert_eq!(observed.len(), footprint.entries.len() + 1);
        for key in [
            "global.db",
            "user-sessions.db",
            "global.db-wal",
            "user-sessions.db-wal",
            "user-memory",
            "user-memory.db-wal",
        ] {
            assert_eq!(
                observed[key].finding().state(),
                DoctorEvidenceStateV1::HealthyCompleteCoverage,
                "{key} must not inherit unrelated suffix bytes"
            );
        }
        for key in [
            "global.db-mystery",
            "user-sessions.db-backup",
            "user-memory.db-backup",
            "user-memory.grafeo.notes",
        ] {
            assert_eq!(
                observed[key].finding().state(),
                DoctorEvidenceStateV1::Degraded,
                "{key} must retain its own named bound"
            );
        }
    }

    #[test]
    fn managed_profile_families_bound_wal_named_payloads() {
        let temp = tempfile::tempdir().expect("footprint fixture");
        let profile = temp.path().join("profile");
        for (name, bytes) in [
            ("lcm-payloads/payload-wal/part", 25),
            ("lcm-payloads/archive.wal", 34),
            ("lcm-payloads/plain", 3),
            ("response-handles/handle-wal/part", 11),
            ("maintenance/cursor.wal", 13),
            ("global.db-wal", 7),
        ] {
            sized(&profile.join(name), bytes);
        }
        let overrides = BTreeMap::from([
            ("lcm-payloads".to_owned(), 61),
            ("response-handles".to_owned(), 10),
            ("maintenance".to_owned(), 12),
            ("global.db-wal".to_owned(), 7),
        ]);
        let footprint = census_profile_footprint(&profile, &BTreeMap::new(), &overrides);
        assert_eq!(footprint.total_bytes, 93);
        assert_eq!(
            footprint.entries.len(),
            4,
            "owner aggregates must not be split"
        );
        assert_eq!(
            footprint.full_profile_size().state,
            ProfileTotalCoverageStateV1::Complete
        );
        for (key, family, bytes) in [
            ("lcm-payloads", ProfileFamilyV1::LcmPayloads, 62),
            ("response-handles", ProfileFamilyV1::ResponseHandles, 11),
            ("maintenance", ProfileFamilyV1::Maintenance, 13),
            ("global.db-wal", ProfileFamilyV1::Wal, 7),
        ] {
            let measured = entry(&footprint, key);
            assert_eq!(measured.family, family, "{key}");
            assert_eq!(measured.bytes, bytes, "{key}");
        }
        let observed = findings(&footprint);
        assert_eq!(observed.len(), footprint.entries.len() + 1);
        for key in ["lcm-payloads", "response-handles", "maintenance"] {
            assert_eq!(
                observed[key].finding().state(),
                DoctorEvidenceStateV1::Degraded,
                "{key} must retain a named over-budget finding"
            );
        }
        for key in ["global.db-wal", "profile"] {
            assert_eq!(
                observed[key].finding().state(),
                DoctorEvidenceStateV1::HealthyCompleteCoverage,
                "{key}"
            );
        }
    }

    #[test]
    fn project_payloads_with_wal_names_retain_only_real_root_wal_overlays() {
        let temp = tempfile::tempdir().expect("footprint fixture");
        let profile = temp.path().join("profile");
        for (relative, bytes) in [
            (
                "projects/proj_payload/code-index-v1/objects/archive.wal",
                40,
            ),
            ("projects/proj_payload/lcm-payloads/payload-wal/chunk", 30),
            ("projects/proj_payload/response-handles/result.wal", 17),
            ("projects/proj_payload/plain", 3),
            ("projects/proj_payload/sessions.db-wal", 7),
        ] {
            sized(&profile.join(relative), bytes);
        }
        let overrides = BTreeMap::from([
            ("projects/proj_payload".to_owned(), 96),
            ("projects/proj_payload/sessions.db-wal".to_owned(), 7),
        ]);
        let footprint = census_profile_footprint(&profile, &BTreeMap::new(), &overrides);
        assert_eq!(footprint.total_bytes, 97);
        assert_eq!(footprint.unavailable_entry_count, 0);
        assert_eq!(
            footprint.entries.len(),
            2,
            "nested payloads cannot be false WALs"
        );
        assert_eq!(
            footprint.full_profile_size().state,
            ProfileTotalCoverageStateV1::Complete
        );
        let project = entry(&footprint, "projects/proj_payload");
        assert_eq!(project.family, ProfileFamilyV1::ProjectStore);
        assert_eq!(project.bytes, 97);
        let kinds = project.kinds.expect("project breakdown");
        assert_eq!(kinds.generation_artifacts, 40);
        assert_eq!(kinds.sessions, 7);
        assert_eq!(kinds.other, 50);
        assert_eq!(kinds.total(), 97);
        let wal = entry(&footprint, "projects/proj_payload/sessions.db-wal");
        assert_eq!(wal.family, ProfileFamilyV1::Wal);
        assert_eq!(wal.bytes, 7);
        let observed = findings(&footprint);
        assert_eq!(observed.len(), 3);
        assert_eq!(
            observed["projects/proj_payload"].finding().state(),
            DoctorEvidenceStateV1::Degraded
        );
        for key in ["projects/proj_payload/sessions.db-wal", PROFILE_TOTAL_KEY] {
            assert_eq!(
                observed[key].finding().state(),
                DoctorEvidenceStateV1::HealthyCompleteCoverage,
                "{key}"
            );
        }
    }

    #[test]
    fn unrelated_profile_trees_keep_wal_named_descendants_under_their_own_bounds() {
        let temp = tempfile::tempdir().expect("footprint fixture");
        let profile = temp.path().join("profile");
        for (relative, bytes) in [
            ("host-bundle-stage/artifact-wal/chunk", 40),
            ("host-bundle-stage/cache/trace.wal", 30),
            ("host-components/bundle.wal", 17),
            ("host-components/plain", 3),
            ("global.db-wal", 7),
        ] {
            sized(&profile.join(relative), bytes);
        }
        let overrides = BTreeMap::from([
            ("host-bundle-stage".to_owned(), 69),
            ("host-components".to_owned(), 19),
            ("global.db-wal".to_owned(), 7),
        ]);
        let footprint = census_profile_footprint(&profile, &BTreeMap::new(), &overrides);
        assert_eq!(footprint.total_bytes, 97);
        assert_eq!(footprint.unavailable_entry_count, 0);
        assert_eq!(footprint.entries.len(), 3);
        assert_eq!(
            footprint.full_profile_size().state,
            ProfileTotalCoverageStateV1::Complete
        );
        for (key, family, bytes) in [
            ("host-bundle-stage", ProfileFamilyV1::Other, 70),
            ("host-components", ProfileFamilyV1::Other, 20),
            ("global.db-wal", ProfileFamilyV1::Wal, 7),
        ] {
            let measured = entry(&footprint, key);
            assert_eq!(measured.family, family, "{key}");
            assert_eq!(measured.bytes, bytes, "{key}");
        }
        let observed = findings(&footprint);
        assert_eq!(observed.len(), 4);
        for key in ["host-bundle-stage", "host-components"] {
            assert_eq!(
                observed[key].finding().state(),
                DoctorEvidenceStateV1::Degraded,
                "{key} must retain its named over-budget finding"
            );
        }
        for key in ["global.db-wal", PROFILE_TOTAL_KEY] {
            assert_eq!(
                observed[key].finding().state(),
                DoctorEvidenceStateV1::HealthyCompleteCoverage,
                "{key} must keep its actual bound"
            );
        }
    }

    #[test]
    fn sqlite_sidecar_named_directories_have_independent_doctor_bounds() {
        let temp = tempfile::tempdir().expect("footprint fixture");
        let profile = temp.path().join("profile");
        // A directory bearing a SQLite sidecar name is not a SQLite file.
        for (relative, bytes) in [
            ("global.db", 5),
            ("user-sessions.db", 7),
            ("user-memory.db", 9),
            ("global.db-journal/incident.bin", 11),
            ("user-memory.db-shm/incident.bin", 13),
            ("user-sessions.db-shm/incident.bin", 17),
        ] {
            sized(&profile.join(relative), bytes);
        }
        let overrides = BTreeMap::from([
            ("global.db".to_owned(), 5),
            ("user-sessions.db".to_owned(), 7),
            ("user-memory".to_owned(), 9),
            ("global.db-journal".to_owned(), 10),
            ("user-memory.db-shm".to_owned(), 12),
            ("user-sessions.db-shm".to_owned(), 16),
        ]);
        let footprint = census_profile_footprint(&profile, &BTreeMap::new(), &overrides);
        assert_eq!(footprint.total_bytes, 62);
        assert_eq!(footprint.unavailable_entry_count, 0);
        assert_eq!(
            footprint.full_profile_size().state,
            ProfileTotalCoverageStateV1::Complete
        );
        let observed = findings(&footprint);
        assert_eq!(observed.len(), footprint.entries.len() + 1);
        for (key, family, bytes) in [
            ("global.db", ProfileFamilyV1::ProfileRegistry, 5),
            ("user-sessions.db", ProfileFamilyV1::UserSessions, 7),
            ("user-memory", ProfileFamilyV1::UserMemory, 9),
            ("global.db-journal", ProfileFamilyV1::Other, 11),
            ("user-memory.db-shm", ProfileFamilyV1::Other, 13),
            ("user-sessions.db-shm", ProfileFamilyV1::Other, 17),
        ] {
            assert_eq!(entry(&footprint, key).family, family, "{key}");
            assert_eq!(entry(&footprint, key).bytes, bytes, "{key}");
        }
        for key in ["global.db", "user-sessions.db", "user-memory", "profile"] {
            assert_eq!(
                observed[key].finding().state(),
                DoctorEvidenceStateV1::HealthyCompleteCoverage,
                "{key}"
            );
        }
        for key in [
            "global.db-journal",
            "user-memory.db-shm",
            "user-sessions.db-shm",
        ] {
            assert_eq!(
                observed[key].finding().state(),
                DoctorEvidenceStateV1::Degraded,
                "{key} must have an independent named over-budget finding"
            );
        }
    }

    #[test]
    fn an_unreadable_walk_root_is_accounted_as_unavailable() {
        let temporary = tempfile::tempdir().expect("walk fixture");
        let file_root = temporary.path().join("not-a-directory");
        std::fs::write(&file_root, [1u8]).expect("fixture file");
        let mut remaining = 10;
        let mut unavailable = 0usize;

        let termination = walk_regular_files_with_budget(
            &file_root,
            &mut remaining,
            &|| None,
            &mut |_, entry| {
                if entry == WalkedEntryV1::Unavailable {
                    unavailable += 1;
                }
            },
        );

        assert_eq!(termination, WalkTerminationV1::Complete);
        assert_eq!(unavailable, 1);
        assert_eq!(remaining, 10);
    }

    #[cfg(unix)]
    #[test]
    fn linked_profile_root_cannot_measure_foreign_storage_as_owned() {
        let temporary = tempfile::tempdir().expect("footprint fixture");
        let outside = temporary.path().join("outside");
        let profile = temporary.path().join("profile");
        sized(&outside.join("lcm-payloads/foreign"), 2048);
        std::os::unix::fs::symlink(&outside, &profile).expect("linked profile root");
        let overrides = BTreeMap::from([("lcm-payloads".to_owned(), 1)]);

        let linked = census_profile_footprint(&profile, &BTreeMap::new(), &overrides);
        assert_eq!(linked.total_bytes, 0, "foreign bytes must not be owned");
        assert_eq!(linked.unavailable_entry_count, 1);
        assert_eq!(linked.entries, Vec::<ProfileFootprintEntryV1>::new());
        assert!(!linked.truncated);
        assert_eq!(
            linked.full_profile_size().state,
            ProfileTotalCoverageStateV1::Partial
        );
        let linked_findings = findings(&linked);
        assert_eq!(linked_findings.len(), 1);
        assert_eq!(
            linked_findings["profile"].finding().state(),
            DoctorEvidenceStateV1::Partial,
            "unverifiable root must not be reported healthy"
        );

        // The same bytes must be measured and over-budget when the actual
        // directory is explicitly selected instead of its symbolic link.
        let owned = census_profile_footprint(&outside, &BTreeMap::new(), &overrides);
        assert_eq!(owned.total_bytes, 2048);
        assert_eq!(owned.unavailable_entry_count, 0);
        assert_eq!(entry(&owned, "lcm-payloads").bytes, 2048);
        assert_eq!(
            findings(&owned)["lcm-payloads"].finding().state(),
            DoctorEvidenceStateV1::Degraded
        );
    }

    #[test]
    fn sqlite_wal_lookalike_directories_keep_independent_bounds() {
        let temp = tempfile::tempdir().expect("footprint fixture");
        let profile = temp.path().join("profile");
        for (relative, bytes) in [
            ("global.db", 5),
            ("global.db-wal/incident.bin", 11),
            ("scratch.wal/chunk", 13),
            ("user-sessions.db-wal", 7),
            ("projects/proj_wal/sessions.db-wal", 17),
            ("projects/proj_wal/tracedecay.db-wal/incident.bin", 19),
            ("projects/proj_wal/tracedecay.grafeo.wal/segment", 23),
        ] {
            sized(&profile.join(relative), bytes);
        }
        let overrides = BTreeMap::from([
            ("global.db-wal".to_owned(), 10),
            ("scratch.wal".to_owned(), 12),
            ("projects/proj_wal".to_owned(), 58),
            ("user-sessions.db-wal".to_owned(), 7),
            ("projects/proj_wal/sessions.db-wal".to_owned(), 17),
            ("projects/proj_wal/tracedecay.grafeo.wal".to_owned(), 23),
            (PROFILE_TOTAL_KEY.to_owned(), 95),
        ]);
        let footprint = census_profile_footprint(&profile, &BTreeMap::new(), &overrides);
        assert_eq!(footprint.total_bytes, 95);
        assert_eq!(footprint.unavailable_entry_count, 0);
        assert_eq!(footprint.entries.len(), 7);
        assert_eq!(
            footprint.full_profile_size().state,
            ProfileTotalCoverageStateV1::Complete
        );
        for (key, family, bytes) in [
            ("global.db", ProfileFamilyV1::ProfileRegistry, 5),
            ("global.db-wal", ProfileFamilyV1::Other, 11),
            ("scratch.wal", ProfileFamilyV1::Other, 13),
            ("user-sessions.db-wal", ProfileFamilyV1::Wal, 7),
            ("projects/proj_wal", ProfileFamilyV1::ProjectStore, 59),
            (
                "projects/proj_wal/sessions.db-wal",
                ProfileFamilyV1::Wal,
                17,
            ),
            (
                "projects/proj_wal/tracedecay.grafeo.wal",
                ProfileFamilyV1::Wal,
                23,
            ),
        ] {
            assert_eq!(entry(&footprint, key).family, family, "{key}");
            assert_eq!(entry(&footprint, key).bytes, bytes, "{key}");
        }
        let kinds = entry(&footprint, "projects/proj_wal")
            .kinds
            .expect("production project-kind breakdown");
        assert_eq!(kinds.sessions, 17);
        assert_eq!(kinds.graph_container, 23);
        assert_eq!(kinds.other, 19);
        assert_eq!(kinds.total(), 59);

        let observed = findings(&footprint);
        assert_eq!(observed.len(), 8);
        for key in ["global.db-wal", "scratch.wal", "projects/proj_wal"] {
            assert_eq!(
                observed[key].finding().state(),
                DoctorEvidenceStateV1::Degraded,
                "{key} must not evade its physical footprint bound"
            );
        }
        for key in [
            "global.db",
            "user-sessions.db-wal",
            "projects/proj_wal/sessions.db-wal",
            "projects/proj_wal/tracedecay.grafeo.wal",
            PROFILE_TOTAL_KEY,
        ] {
            assert_eq!(
                observed[key].finding().state(),
                DoctorEvidenceStateV1::HealthyCompleteCoverage,
                "{key} must retain its real, independent bound"
            );
        }
    }

    #[test]
    fn long_footprint_details_keep_doctor_size_and_bound_visible() {
        let temporary = tempfile::tempdir().unwrap();
        let profile = temporary.path().join("profile");
        let key = "projects/proj_long";
        let bytes = 3 * 1024;
        sized(&profile.join(key).join("tracedecay.db"), bytes);
        let registered = BTreeMap::from([(
            "proj_long".to_owned(),
            temporary.path().display().to_string(),
        )]);
        let overrides = BTreeMap::from([(key.to_owned(), 1)]);
        let mut footprint = census_profile_footprint(&profile, &registered, &overrides);
        let project = footprint
            .entries
            .iter_mut()
            .find(|entry| entry.key == key)
            .expect("measured project");
        assert_eq!(
            project.project.as_ref().unwrap().state,
            ProjectRootStateV1::Present
        );
        // The complete state remains available in the storage report; only
        // the Doctor coverage text is bounded.
        project.project.as_mut().unwrap().registered_root = Some("long-root/".repeat(120));
        let observed = findings(&footprint);
        let finding = &observed[key];
        let statement = finding.finding().coverage().statement();
        assert_eq!(finding.finding().state(), DoctorEvidenceStateV1::Degraded);
        assert!(statement.contains("root present"), "{statement}");
        assert!(
            statement.ends_with(&format!("{} of 1 B bound", format_bytes(bytes))),
            "the measured overage must not be clipped after a long root: {statement}"
        );
        assert!(!statement.contains("long-root/long-root"), "{statement}");

        // A valid long evidence key can also fill the statement budget before
        // the numeric overage. Hash the reference, never the reported bytes.
        let project = footprint
            .entries
            .iter_mut()
            .find(|entry| entry.key == key)
            .expect("measured project");
        project.key = format!("projects/proj_{}/graph.grafeo", "x".repeat(520));
        let hash_key = format!("footprint/sha256:{}", sha256_hex(project.key.as_bytes()));
        let observed = findings(&footprint);
        let finding = &observed[&hash_key];
        let statement = finding.finding().coverage().statement();
        assert_eq!(finding.finding().state(), DoctorEvidenceStateV1::Degraded);
        assert!(
            statement.contains("full path in storage report"),
            "{statement}"
        );
        assert!(
            statement.ends_with(&format!("{} of 1 B bound", format_bytes(bytes))),
            "the complete numeric bound must survive a long evidence key: {statement}"
        );
        assert_eq!(
            std::fs::metadata(profile.join(key).join("tracedecay.db"))
                .unwrap()
                .len(),
            bytes
        );
    }
}
