//! Incident-debris detection and deletion (Plan 38 §5).

use std::io;
use std::path::{Path, PathBuf};

#[cfg(not(windows))]
use cap_fs_ext::OpenOptionsMaybeDirExt;
use cap_fs_ext::ambient_authority;
use cap_std::fs::Dir;
#[cfg(not(windows))]
use cap_std::fs::OpenOptions;
use tracedecay_contracts::storage::{
    IncidentDebrisArtifactV1, IncidentDebrisKindV1, IncidentDebrisScanV1, RelativeArtifactPathV1,
    StorageByteSizeV1, StoreKeyV1,
};
use tracedecay_domain::UtcMicros;

use super::orphan_stores::StoreCensusEntry;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IncidentDebrisFailureKind {
    OutsideProfile,
    InspectFailed,
    RemoveFailed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncidentDebrisFailure {
    pub store_id: String,
    pub kind: IncidentDebrisFailureKind,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IncidentDebrisSweepReport {
    pub collected: usize,
    pub reclaimed_bytes: u64,
    pub errors: Vec<IncidentDebrisFailure>,
}

struct StoreDebrisCapability {
    store_id: String,
    root: Dir,
}

/// Resolves the owner profile once per sweep: the containment fence below
/// compares every store against it, and the answer cannot change mid-sweep.
fn canonical_profile_root(profile_root: &Path) -> Result<PathBuf, IncidentDebrisFailureKind> {
    profile_root
        .canonicalize()
        .map_err(|_| IncidentDebrisFailureKind::InspectFailed)
}

impl StoreDebrisCapability {
    /// `profile` must already be canonical (see [`canonical_profile_root`]).
    fn open(entry: &StoreCensusEntry, profile: &Path) -> Result<Self, IncidentDebrisFailureKind> {
        let store = entry
            .data_root
            .canonicalize()
            .map_err(|_| IncidentDebrisFailureKind::InspectFailed)?;
        if store == profile || !store.starts_with(profile) {
            return Err(IncidentDebrisFailureKind::OutsideProfile);
        }
        let root = Dir::open_ambient_dir(&store, ambient_authority())
            .map_err(|_| IncidentDebrisFailureKind::InspectFailed)?;
        Ok(Self {
            store_id: entry.store_id.clone(),
            root,
        })
    }
}

/// Deletes every classified loose debris file beside each census store.
/// Only regular files reached through the store capability without following
/// symlinks are removed; directories are never debris.
#[must_use]
#[hotpath::measure(label = "maintenance.incident_debris.sweep")]
pub fn sweep_incident_debris(
    census: &[StoreCensusEntry],
    profile_root: &Path,
) -> IncidentDebrisSweepReport {
    let mut report = IncidentDebrisSweepReport::default();
    let profile = match canonical_profile_root(profile_root) {
        Ok(profile) => profile,
        Err(kind) => {
            report
                .errors
                .extend(census.iter().map(|entry| failure(entry, kind)));
            return observed_sweep_report(report);
        }
    };
    for entry in census {
        match StoreDebrisCapability::open(entry, &profile) {
            Ok(capability) => delete_loose_debris(&capability, &mut report),
            Err(kind) => report.errors.push(failure(entry, kind)),
        }
    }
    observed_sweep_report(report)
}

/// Items-removed census for the one outer sweep wall span, including the
/// fail-closed early exits that touch nothing but report every store.
fn observed_sweep_report(report: IncidentDebrisSweepReport) -> IncidentDebrisSweepReport {
    hotpath::gauge!("maintenance.incident_debris.collected_total").inc(report.collected);
    hotpath::gauge!("maintenance.incident_debris.failed_total").inc(report.errors.len());
    hotpath::gauge!("maintenance.incident_debris.reclaimed_bytes_total")
        .inc(report.reclaimed_bytes);
    report
}

#[hotpath::measure(label = "maintenance.incident_debris.scan")]
pub fn scan_incident_debris(
    entry: &StoreCensusEntry,
    profile_root: &Path,
    now: i64,
) -> Result<IncidentDebrisScanV1, IncidentDebrisFailureKind> {
    let capability = StoreDebrisCapability::open(entry, &canonical_profile_root(profile_root)?)?;
    let store = StoreKeyV1::new(entry.store_id.clone())
        .map_err(|_| IncidentDebrisFailureKind::InspectFailed)?;
    let observed_at = UtcMicros(now.saturating_mul(1_000_000));
    let mut artifacts = Vec::new();
    let mut listing_complete = true;

    let entries = capability
        .root
        .read_dir(".")
        .map_err(|_| IncidentDebrisFailureKind::InspectFailed)?;
    for listed in entries {
        let listed = match listed {
            Ok(listed) => listed,
            Err(_) => {
                listing_complete = false;
                continue;
            }
        };
        let name = listed.file_name();
        let Some(name) = name.to_str() else {
            listing_complete = false;
            continue;
        };
        let file_type = match listed.file_type() {
            Ok(file_type) => file_type,
            Err(_) => {
                listing_complete = false;
                continue;
            }
        };
        // Store subdirectories are never debris; anything that is neither a
        // directory nor a regular file makes the listing partial.
        if file_type.is_dir() {
            continue;
        }
        if !file_type.is_file() {
            listing_complete = false;
            continue;
        }
        let Some(kind) = IncidentDebrisKindV1::classify(name) else {
            continue;
        };
        let size_bytes = match listed.metadata() {
            Ok(metadata) => metadata.len(),
            Err(_) => {
                listing_complete = false;
                continue;
            }
        };
        if let Some(artifact) = application_artifact(&store, name, kind, size_bytes, observed_at) {
            artifacts.push(artifact);
        } else {
            listing_complete = false;
        }
    }

    Ok(IncidentDebrisScanV1 {
        store,
        artifacts,
        listing_complete,
    })
}

fn failure(entry: &StoreCensusEntry, kind: IncidentDebrisFailureKind) -> IncidentDebrisFailure {
    IncidentDebrisFailure {
        store_id: entry.store_id.clone(),
        kind,
    }
}

fn push_failure(
    report: &mut IncidentDebrisSweepReport,
    store_id: &str,
    kind: IncidentDebrisFailureKind,
) {
    report.errors.push(IncidentDebrisFailure {
        store_id: store_id.to_string(),
        kind,
    });
}

fn delete_loose_debris(capability: &StoreDebrisCapability, report: &mut IncidentDebrisSweepReport) {
    let entries = match capability.root.read_dir(".") {
        Ok(entries) => entries,
        Err(_) => {
            push_failure(
                report,
                &capability.store_id,
                IncidentDebrisFailureKind::InspectFailed,
            );
            return;
        }
    };
    let mut debris = Vec::new();
    for listed in entries {
        let Ok(listed) = listed else {
            push_failure(
                report,
                &capability.store_id,
                IncidentDebrisFailureKind::InspectFailed,
            );
            continue;
        };
        let name = listed.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if IncidentDebrisKindV1::classify(name).is_none() {
            continue;
        }
        // `DirEntry` metadata describes the entry itself, so a symlink named
        // like debris is never treated as a regular file.
        match listed.metadata() {
            Ok(metadata) if metadata.is_file() => debris.push((name.to_string(), metadata.len())),
            Ok(_) => {}
            Err(_) => push_failure(
                report,
                &capability.store_id,
                IncidentDebrisFailureKind::InspectFailed,
            ),
        }
    }
    let mut removed = false;
    for (name, size_bytes) in debris {
        if capability.root.remove_file(&name).is_err() {
            push_failure(
                report,
                &capability.store_id,
                IncidentDebrisFailureKind::RemoveFailed,
            );
            continue;
        }
        removed = true;
        report.collected = report.collected.saturating_add(1);
        report.reclaimed_bytes = report.reclaimed_bytes.saturating_add(size_bytes);
    }
    if removed && sync_dir(&capability.root).is_err() {
        push_failure(
            report,
            &capability.store_id,
            IncidentDebrisFailureKind::RemoveFailed,
        );
    }
}

fn application_artifact(
    store: &StoreKeyV1,
    name: &str,
    kind: IncidentDebrisKindV1,
    size_bytes: u64,
    observed_at: UtcMicros,
) -> Option<IncidentDebrisArtifactV1> {
    let path = RelativeArtifactPathV1::new(name.to_string()).ok()?;
    IncidentDebrisArtifactV1::classify_path(
        store.clone(),
        path,
        StorageByteSizeV1(size_bytes),
        observed_at,
    )
    .ok()
    .flatten()
    .filter(|artifact| artifact.kind == kind)
}

fn sync_dir(directory: &Dir) -> io::Result<()> {
    #[cfg(windows)]
    {
        directory.dir_metadata().map(|_| ())
    }
    #[cfg(not(windows))]
    {
        let mut options = OpenOptions::new();
        options.read(true).maybe_dir(true);
        directory
            .open_with(".", &options)
            .and_then(|file| file.sync_all())
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use tracedecay_contracts::storage::IncidentDebrisKindV1;

    use super::*;
    use crate::retention::orphan_stores::{
        StoreCensusEntry, StoreContentFence, StoreDirectoryFence,
    };

    const NOW: i64 = 1_800_000_000;

    fn entry(store_root: &Path) -> StoreCensusEntry {
        StoreCensusEntry {
            project_id: "project.debris".to_string(),
            store_id: "store.debris".to_string(),
            canonical_root: PathBuf::from("/repository"),
            display_root: None,
            git_common_dir: None,
            alias_roots: Vec::new(),
            manifest_readable: false,
            data_root: store_root.to_path_buf(),
            manifest_root: None,
            last_write_secs: NOW,
            size_bytes: 0,
            expected_store_relpath: "stores/store.debris".to_string(),
            expected_created_at: 0,
            expected_last_write_at: None,
            expected_payload_mtime_secs: NOW,
            expected_data_root_fence: StoreDirectoryFence::Missing,
            expected_content_fence: StoreContentFence::Missing,
            expected_manifest_bytes: None,
            graph_scope_relpaths: Vec::new(),
        }
    }

    #[test]
    fn sweep_deletes_loose_debris_immediately_and_preserves_live_files() {
        let profile = tempfile::tempdir().unwrap();
        let store_root = profile.path().join("stores/store.debris");
        std::fs::create_dir_all(store_root.join("payloads")).unwrap();
        let debris = store_root.join("sessions.db.corrupt-incident");
        let scratch = store_root.join("recovery-scratch-incident");
        let live = store_root.join("sessions.db");
        let live_wal = store_root.join("sessions.db-wal");
        std::fs::write(&debris, b"debris payload").unwrap();
        std::fs::write(&scratch, b"scratch").unwrap();
        std::fs::write(&live, b"live database").unwrap();
        std::fs::write(&live_wal, b"live wal").unwrap();

        let before = scan_incident_debris(&entry(&store_root), profile.path(), NOW).unwrap();
        let mut kinds = before
            .artifacts
            .iter()
            .map(|artifact| artifact.kind)
            .collect::<Vec<_>>();
        kinds.sort();
        assert_eq!(
            kinds,
            [
                IncidentDebrisKindV1::Corrupt,
                IncidentDebrisKindV1::RecoveryScratch
            ]
        );

        let report = sweep_incident_debris(&[entry(&store_root)], profile.path());

        assert_eq!(report.collected, 2);
        assert_eq!(
            report.reclaimed_bytes,
            (b"debris payload".len() + b"scratch".len()) as u64
        );
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(!debris.exists() && !scratch.exists());
        assert_eq!(std::fs::read(&live).unwrap(), b"live database");
        assert_eq!(std::fs::read(&live_wal).unwrap(), b"live wal");
        assert!(store_root.join("payloads").is_dir());
        let entries = std::fs::read_dir(&store_root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 3, "no copy of the debris is left behind");

        let after = scan_incident_debris(&entry(&store_root), profile.path(), NOW).unwrap();
        assert!(after.listing_complete);
        assert!(after.is_empty());
    }

    #[test]
    fn directories_are_never_debris_even_with_a_debris_name() {
        let profile = tempfile::tempdir().unwrap();
        let store_root = profile.path().join("stores/store.debris");
        let named = store_root.join("graph.db.recovered-dir");
        std::fs::create_dir_all(&named).unwrap();
        std::fs::write(named.join("payload"), b"store data").unwrap();

        let scan = scan_incident_debris(&entry(&store_root), profile.path(), NOW).unwrap();
        assert!(scan.listing_complete);
        assert!(scan.is_empty());

        let report = sweep_incident_debris(&[entry(&store_root)], profile.path());
        assert_eq!(report.collected, 0);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(std::fs::read(named.join("payload")).unwrap(), b"store data");
    }

    #[cfg(unix)]
    #[test]
    fn sweep_never_follows_a_debris_named_symlink() {
        let profile = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let store_root = profile.path().join("stores/store.debris");
        std::fs::create_dir_all(&store_root).unwrap();
        let target = outside.path().join("precious.db");
        std::fs::write(&target, b"not debris").unwrap();
        let link = store_root.join("graph.db.recovered-link");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let report = sweep_incident_debris(&[entry(&store_root)], profile.path());

        assert_eq!(report.collected, 0);
        assert!(link.symlink_metadata().is_ok());
        assert_eq!(std::fs::read(&target).unwrap(), b"not debris");
    }

    #[test]
    fn sweep_rejects_store_capability_outside_the_owner_profile() {
        let profile = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let debris = outside.path().join("sessions.db.corrupt-outside");
        std::fs::write(&debris, b"outside").unwrap();

        let report = sweep_incident_debris(&[entry(outside.path())], profile.path());

        assert_eq!(report.collected, 0);
        assert_eq!(
            report.errors,
            vec![IncidentDebrisFailure {
                store_id: "store.debris".to_string(),
                kind: IncidentDebrisFailureKind::OutsideProfile,
            }]
        );
        assert!(debris.exists());
    }
}
