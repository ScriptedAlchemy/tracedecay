//! Profile footprint census: one metadata-only walk of a profile root that
//! sizes every store family against a bound derived from the store's purpose.
//!
//! The walk stats entries and never reads file contents or follows symlinks,
//! so its cost scales with the number of directory entries, not with bytes.
//! That is why the ceiling is an entry count: a byte budget small enough to be
//! cheap is smaller than one real graph container.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracedecay_contracts::doctor::{
    DoctorCoverageCompletenessV1, DoctorCoverageStatementV1, DoctorEvidenceRefV1,
    DoctorEvidenceReferenceV1, DoctorEvidenceStateV1, DoctorFindingFamilyV1, DoctorFindingV1,
    DoctorStorageFamilyReadV1, DoctorStorageFindingKindV1, DoctorStorageFindingV1,
    storage_family_read,
};
use tracedecay_global_db::registry_maintenance::{RootLivenessV1, probe_root};
use tracedecay_runtime_core::text::format_bytes;

use super::storage_report::{
    FullProfileSizeV1, GRAPH_CONTAINER_SUFFIX, ProfileTotalCoverageStateV1, StoreKindBytesV1,
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

/// Visit every regular file under `root` with its byte length, without
/// following symlinks or reading contents. Stops after `entry_ceiling`
/// directory entries and returns whether it did, which makes any total built
/// from the visits a floor.
pub fn walk_regular_files(
    root: &Path,
    entry_ceiling: usize,
    visit: &mut dyn FnMut(&Path, WalkedEntryV1),
) -> bool {
    let mut visited = 0usize;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            visit(&directory, WalkedEntryV1::Unavailable);
            continue;
        };
        for entry in entries {
            if visited >= entry_ceiling {
                return true;
            }
            visited += 1;
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
                match entry.metadata() {
                    Ok(metadata) => visit(
                        &path,
                        WalkedEntryV1::File {
                            bytes: metadata.len(),
                        },
                    ),
                    Err(_) => visit(&path, WalkedEntryV1::Unavailable),
                }
            } else {
                visit(&path, WalkedEntryV1::Unavailable);
            }
        }
    }
    false
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
    census_with_entry_ceiling(
        profile_root,
        registered_roots,
        bound_overrides,
        MAX_FOOTPRINT_ENTRIES,
    )
}

#[derive(Default)]
struct EntryTally {
    bytes: u64,
    unavailable_entry_count: usize,
    kinds: Option<StoreKindBytesV1>,
}

struct FootprintKeys {
    primary: (String, ProfileFamilyV1),
    overlay: Option<(String, ProfileFamilyV1)>,
    /// For a file in a project store: the project id and the store-relative path.
    project: Option<(String, PathBuf)>,
}

fn census_with_entry_ceiling(
    profile_root: &Path,
    registered_roots: &BTreeMap<String, String>,
    bound_overrides: &BTreeMap<String, u64>,
    entry_ceiling: usize,
) -> ProfileFootprintV1 {
    let mut tallies: BTreeMap<(String, ProfileFamilyV1), EntryTally> = BTreeMap::new();
    let mut total_bytes = 0u64;
    let mut unavailable_entry_count = 0usize;
    let truncated = walk_regular_files(profile_root, entry_ceiling, &mut |path, walked| {
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
        let primary = tallies.entry(keys.primary).or_default();
        tally(primary, walked);
        if let (Some((_, store_relative)), WalkedEntryV1::File { bytes }) = (&keys.project, walked)
        {
            primary
                .kinds
                .get_or_insert_with(StoreKindBytesV1::default)
                .add(store_relative, bytes);
        }
        if let Some(overlay) = keys.overlay {
            tally(tallies.entry(overlay).or_default(), walked);
        }
    });
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
                bound_bytes: bound_overrides
                    .get(&key)
                    .copied()
                    .unwrap_or_else(|| default_bound_bytes(family)),
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
        truncated,
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
    for fd in std::fs::read_dir(fd_directory).ok()?.flatten() {
        let Ok(target) = std::fs::read_link(fd.path()) else {
            continue;
        };
        let unlinked_temp = target
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("etilqs_") && name.ends_with(" (deleted)"));
        // An fd closed since the listing no longer holds bytes.
        if let (true, Ok(metadata)) = (unlinked_temp, std::fs::metadata(fd.path())) {
            bytes = bytes.saturating_add(metadata.len());
        }
    }
    Some(ProfileFootprintEntryV1 {
        key: SQLITE_TEMP_SPILL_KEY.to_owned(),
        family: ProfileFamilyV1::SqliteTempSpill,
        bytes,
        unavailable_entry_count: 0,
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

/// The footprint keys one profile-relative path belongs to. A WAL directory
/// is one entry keyed by the directory; its segments sum into it.
fn footprint_keys(relative: &Path) -> Option<FootprintKeys> {
    let components = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let first = components.first()?;
    if first == PROJECTS_DIRECTORY && components.len() >= 3 {
        let store_key = format!("{PROJECTS_DIRECTORY}/{}", components[1]);
        let inner = &components[2..];
        let overlay = inner
            .iter()
            .position(|name| is_wal(name))
            .map(|end| {
                (
                    format!("{store_key}/{}", inner[..=end].join("/")),
                    ProfileFamilyV1::Wal,
                )
            })
            .or_else(|| {
                (inner.len() == 1 && inner[0].ends_with(GRAPH_CONTAINER_SUFFIX)).then(|| {
                    (
                        format!("{store_key}/{}", inner[0]),
                        ProfileFamilyV1::GraphContainer,
                    )
                })
            });
        return Some(FootprintKeys {
            primary: (store_key, ProfileFamilyV1::ProjectStore),
            overlay,
            project: Some((components[1].clone(), inner.iter().collect())),
        });
    }
    let primary = if let Some(end) = components.iter().position(|name| is_wal(name)) {
        (components[..=end].join("/"), ProfileFamilyV1::Wal)
    } else if let Some(stem) = first
        .find(GRAPH_CONTAINER_SUFFIX)
        .map(|at| &first[..at + GRAPH_CONTAINER_SUFFIX.len()])
    {
        // A top-level container and its sidecars (`.verified`, `.spill`).
        (stem.to_owned(), ProfileFamilyV1::GraphContainer)
    } else if first == "global.db" || first.starts_with("global.db-") {
        ("global.db".to_owned(), ProfileFamilyV1::ProfileRegistry)
    } else if first.starts_with("user-sessions.db") {
        ("user-sessions.db".to_owned(), ProfileFamilyV1::UserSessions)
    } else if first.starts_with("user-memory.") {
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

/// One `OverBudgetStore` finding per footprint entry plus one for the profile
/// total. A floor above its bound proves the overage, so an over-bound entry
/// is degraded with complete coverage even when bytes went unread.
#[must_use]
pub fn footprint_findings(footprint: &ProfileFootprintV1) -> DoctorStorageFamilyReadV1 {
    let mut findings = Vec::with_capacity(footprint.entries.len() + 1);
    for entry in &footprint.entries {
        let Some(finding) = bounded_finding(
            &entry.key,
            &entry_statement(entry),
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

fn entry_statement(entry: &ProfileFootprintEntryV1) -> String {
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
    format!(
        "{label} {}{root}{breakdown}{}",
        entry.key,
        floor_note(entry.unavailable_entry_count, false)
    )
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

        let entry = census_sqlite_temp_spill(&fd_directory, &BTreeMap::new())
            .expect("a readable fd directory");

        assert_eq!(entry.key, SQLITE_TEMP_SPILL_KEY);
        assert_eq!(entry.bytes, 1_924 * MIB);
        assert_eq!(entry.bound_bytes, GIB);
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
        let footprint =
            census_with_entry_ceiling(&fixture.profile, &fixture.registered, &BTreeMap::new(), 3);

        assert!(footprint.truncated);
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
}
