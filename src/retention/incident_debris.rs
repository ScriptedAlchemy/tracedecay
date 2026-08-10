//! Durable incident-debris quarantine and retention collection (Plan 38 §5).

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[cfg(not(windows))]
use cap_fs_ext::OpenOptionsMaybeDirExt;
use cap_fs_ext::{
    DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt, ambient_authority,
};
use cap_std::fs::{Dir, DirBuilder, OpenOptions};
#[cfg(unix)]
use cap_std::fs::{DirBuilderExt, OpenOptionsExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_application::storage::{
    IncidentDebrisArtifactV1, IncidentDebrisKindV1, IncidentDebrisScanV1, RelativeArtifactPathV1,
    StorageByteSizeV1, StoreKeyV1,
};
use tracedecay_domain::UtcMicros;

use super::orphan_stores::StoreCensusEntry;

pub const INCIDENT_DEBRIS_QUARANTINE_DIR: &str = ".incident-debris";
const METADATA_SCHEMA_V1: &str = "tracedecay.incident-debris.v1";
const HASH_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncidentDebrisMetadataV1 {
    pub schema: String,
    pub record_id: String,
    pub store_id: String,
    pub original_name: String,
    pub kind: IncidentDebrisKindV1,
    pub content_sha256: String,
    pub size_bytes: u64,
    pub quarantined_at_secs: i64,
    pub collection_eligible_at_secs: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IncidentDebrisFailureKind {
    OutsideProfile,
    InspectFailed,
    MetadataInvalid,
    MetadataWriteFailed,
    MoveFailed,
    IntegrityMismatch,
    RemoveFailed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncidentDebrisFailure {
    pub store_id: String,
    pub kind: IncidentDebrisFailureKind,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IncidentDebrisSweepReport {
    pub quarantined: usize,
    pub collected: usize,
    pub retained: usize,
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

    fn quarantine_dir(&self, create: bool) -> io::Result<Option<Dir>> {
        match self.root.open_dir_nofollow(INCIDENT_DEBRIS_QUARANTINE_DIR) {
            Ok(directory) => Ok(Some(directory)),
            Err(error) if error.kind() == io::ErrorKind::NotFound && !create => Ok(None),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                #[allow(unused_mut)]
                let mut builder = DirBuilder::new();
                #[cfg(unix)]
                builder.mode(0o700);
                match self
                    .root
                    .create_dir_with(INCIDENT_DEBRIS_QUARANTINE_DIR, &builder)
                {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
                self.root
                    .open_dir_nofollow(INCIDENT_DEBRIS_QUARANTINE_DIR)
                    .map(Some)
            }
            Err(error) => Err(error),
        }
    }
}

#[must_use]
pub fn sweep_incident_debris(
    census: &[StoreCensusEntry],
    profile_root: &Path,
    retention_secs: i64,
    now: i64,
) -> IncidentDebrisSweepReport {
    let mut report = IncidentDebrisSweepReport::default();
    if retention_secs <= 0 {
        report
            .errors
            .extend(census.iter().map(|entry| IncidentDebrisFailure {
                store_id: entry.store_id.clone(),
                kind: IncidentDebrisFailureKind::MetadataInvalid,
            }));
        return report;
    }
    let profile = match canonical_profile_root(profile_root) {
        Ok(profile) => profile,
        Err(kind) => {
            report
                .errors
                .extend(census.iter().map(|entry| failure(entry, kind)));
            return report;
        }
    };
    for entry in census {
        let capability = match StoreDebrisCapability::open(entry, &profile) {
            Ok(capability) => capability,
            Err(kind) => {
                report.errors.push(failure(entry, kind));
                continue;
            }
        };
        quarantine_loose(&capability, retention_secs, now, &mut report);
        collect_due(&capability, now, &mut report);
        report.retained = report
            .retained
            .saturating_add(retained_count(&capability, &mut report.errors));
    }
    report
}

pub fn scan_incident_debris(
    entry: &StoreCensusEntry,
    profile_root: &Path,
    now: i64,
) -> Result<IncidentDebrisScanV1, IncidentDebrisFailureKind> {
    let capability = StoreDebrisCapability::open(entry, &canonical_profile_root(profile_root)?)?;
    let store = StoreKeyV1::new(entry.store_id.clone())
        .map_err(|_| IncidentDebrisFailureKind::MetadataInvalid)?;
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
        if name == INCIDENT_DEBRIS_QUARANTINE_DIR {
            continue;
        }
        let file_type = match listed.file_type() {
            Ok(file_type) => file_type,
            Err(_) => {
                listing_complete = false;
                continue;
            }
        };
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
        let Ok(metadata) = listed.metadata() else {
            listing_complete = false;
            continue;
        };
        if let Some(artifact) =
            application_artifact(&store, name, kind, metadata.len(), observed_at)
        {
            artifacts.push(artifact);
        } else {
            listing_complete = false;
        }
    }

    if let Some(quarantine) = capability
        .quarantine_dir(false)
        .map_err(|_| IncidentDebrisFailureKind::InspectFailed)?
    {
        for metadata_name in metadata_names(&quarantine, &mut listing_complete) {
            let metadata = match read_metadata(&quarantine, &metadata_name) {
                Ok(metadata) if validate_metadata(&metadata, &capability.store_id) => metadata,
                _ => {
                    listing_complete = false;
                    continue;
                }
            };
            let artifact_name = artifact_name(&metadata.record_id);
            match quarantine.symlink_metadata(&artifact_name) {
                Ok(file) if file.is_file() => {}
                _ => {
                    listing_complete = false;
                    continue;
                }
            }
            if let Some(artifact) = application_artifact(
                &store,
                &metadata.original_name,
                metadata.kind,
                metadata.size_bytes,
                UtcMicros(metadata.quarantined_at_secs.saturating_mul(1_000_000)),
            ) {
                artifacts.push(artifact);
            } else {
                listing_complete = false;
            }
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

fn quarantine_loose(
    capability: &StoreDebrisCapability,
    retention_secs: i64,
    now: i64,
    report: &mut IncidentDebrisSweepReport,
) {
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
    let mut names = Vec::new();
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
        if listed.file_type().is_ok_and(|kind| kind.is_file())
            && IncidentDebrisKindV1::classify(name).is_some()
        {
            names.push(name.to_string());
        }
    }
    names.sort();

    for name in names {
        let Some(kind) = IncidentDebrisKindV1::classify(&name) else {
            continue;
        };
        let record = match quarantine_record(capability, &name, kind, retention_secs, now) {
            Ok(record) => record,
            Err(kind) => {
                push_failure(report, &capability.store_id, kind);
                continue;
            }
        };
        let quarantine = match capability.quarantine_dir(true) {
            Ok(Some(quarantine)) => quarantine,
            _ => {
                push_failure(
                    report,
                    &capability.store_id,
                    IncidentDebrisFailureKind::MetadataWriteFailed,
                );
                continue;
            }
        };
        let metadata_name = metadata_name(&record.record_id);
        if write_metadata(&quarantine, &metadata_name, &record).is_err() {
            push_failure(
                report,
                &capability.store_id,
                IncidentDebrisFailureKind::MetadataWriteFailed,
            );
            continue;
        }
        let artifact_name = artifact_name(&record.record_id);
        if quarantine.symlink_metadata(&artifact_name).is_ok() {
            push_failure(
                report,
                &capability.store_id,
                IncidentDebrisFailureKind::MoveFailed,
            );
            continue;
        }
        if capability
            .root
            .rename(&name, &quarantine, &artifact_name)
            .is_err()
        {
            let _ = quarantine.remove_file(&metadata_name);
            let _ = sync_dir(&quarantine);
            push_failure(
                report,
                &capability.store_id,
                IncidentDebrisFailureKind::MoveFailed,
            );
            continue;
        }
        if verify_artifact(&quarantine, &artifact_name, &record).is_err() {
            let _ = quarantine.rename(&artifact_name, &capability.root, &name);
            let _ = quarantine.remove_file(&metadata_name);
            let _ = sync_dir(&quarantine);
            let _ = sync_dir(&capability.root);
            push_failure(
                report,
                &capability.store_id,
                IncidentDebrisFailureKind::IntegrityMismatch,
            );
            continue;
        }
        if sync_dir(&quarantine).is_err() || sync_dir(&capability.root).is_err() {
            push_failure(
                report,
                &capability.store_id,
                IncidentDebrisFailureKind::MoveFailed,
            );
            continue;
        }
        report.quarantined = report.quarantined.saturating_add(1);
    }
}

fn collect_due(
    capability: &StoreDebrisCapability,
    now: i64,
    report: &mut IncidentDebrisSweepReport,
) {
    let quarantine = match capability.quarantine_dir(false) {
        Ok(Some(quarantine)) => quarantine,
        Ok(None) => return,
        Err(_) => {
            push_failure(
                report,
                &capability.store_id,
                IncidentDebrisFailureKind::InspectFailed,
            );
            return;
        }
    };
    let mut complete = true;
    for metadata_name in metadata_names(&quarantine, &mut complete) {
        let record = match read_metadata(&quarantine, &metadata_name) {
            Ok(record) if validate_metadata(&record, &capability.store_id) => record,
            _ => {
                push_failure(
                    report,
                    &capability.store_id,
                    IncidentDebrisFailureKind::MetadataInvalid,
                );
                continue;
            }
        };
        if now < record.collection_eligible_at_secs {
            continue;
        }
        let artifact_name = artifact_name(&record.record_id);
        match verify_artifact(&quarantine, &artifact_name, &record) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if quarantine.remove_file(&metadata_name).is_err() || sync_dir(&quarantine).is_err()
                {
                    push_failure(
                        report,
                        &capability.store_id,
                        IncidentDebrisFailureKind::RemoveFailed,
                    );
                }
                continue;
            }
            Err(_) => {
                push_failure(
                    report,
                    &capability.store_id,
                    IncidentDebrisFailureKind::IntegrityMismatch,
                );
                continue;
            }
        }
        if quarantine.remove_file(&artifact_name).is_err() || sync_dir(&quarantine).is_err() {
            push_failure(
                report,
                &capability.store_id,
                IncidentDebrisFailureKind::RemoveFailed,
            );
            continue;
        }
        report.collected = report.collected.saturating_add(1);
        report.reclaimed_bytes = report.reclaimed_bytes.saturating_add(record.size_bytes);
        if quarantine.remove_file(&metadata_name).is_err() || sync_dir(&quarantine).is_err() {
            push_failure(
                report,
                &capability.store_id,
                IncidentDebrisFailureKind::RemoveFailed,
            );
        }
    }
    if !complete {
        push_failure(
            report,
            &capability.store_id,
            IncidentDebrisFailureKind::InspectFailed,
        );
    }
}

fn retained_count(
    capability: &StoreDebrisCapability,
    errors: &mut Vec<IncidentDebrisFailure>,
) -> usize {
    let quarantine = match capability.quarantine_dir(false) {
        Ok(Some(quarantine)) => quarantine,
        Ok(None) => return 0,
        Err(_) => {
            errors.push(IncidentDebrisFailure {
                store_id: capability.store_id.clone(),
                kind: IncidentDebrisFailureKind::InspectFailed,
            });
            return 0;
        }
    };
    let mut complete = true;
    let count = metadata_names(&quarantine, &mut complete)
        .into_iter()
        .filter_map(|name| read_metadata(&quarantine, &name).ok())
        .filter(|metadata| validate_metadata(metadata, &capability.store_id))
        .count();
    if !complete {
        errors.push(IncidentDebrisFailure {
            store_id: capability.store_id.clone(),
            kind: IncidentDebrisFailureKind::InspectFailed,
        });
    }
    count
}

fn quarantine_record(
    capability: &StoreDebrisCapability,
    name: &str,
    kind: IncidentDebrisKindV1,
    retention_secs: i64,
    now: i64,
) -> Result<IncidentDebrisMetadataV1, IncidentDebrisFailureKind> {
    if !is_component(name) {
        return Err(IncidentDebrisFailureKind::InspectFailed);
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let mut file = capability
        .root
        .open_with(name, &options)
        .map_err(|_| IncidentDebrisFailureKind::InspectFailed)?;
    let metadata = file
        .metadata()
        .map_err(|_| IncidentDebrisFailureKind::InspectFailed)?;
    if !metadata.is_file() {
        return Err(IncidentDebrisFailureKind::InspectFailed);
    }
    let content_sha256 =
        sha256_reader(&mut file).map_err(|_| IncidentDebrisFailureKind::InspectFailed)?;
    let modified_nanos = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.into_std().duration_since(UNIX_EPOCH).ok())
        .map_or(0u128, |elapsed| elapsed.as_nanos());
    let mut identity = Sha256::new();
    identity.update(b"tracedecay.incident-debris.record.v1\0");
    identity.update(capability.store_id.as_bytes());
    identity.update(b"\0");
    identity.update(name.as_bytes());
    identity.update(b"\0");
    identity.update(metadata.len().to_le_bytes());
    identity.update(modified_nanos.to_le_bytes());
    identity.update(content_sha256.as_bytes());
    let record_id = hex::encode(identity.finalize());
    Ok(IncidentDebrisMetadataV1 {
        schema: METADATA_SCHEMA_V1.to_string(),
        record_id,
        store_id: capability.store_id.clone(),
        original_name: name.to_string(),
        kind,
        content_sha256,
        size_bytes: metadata.len(),
        quarantined_at_secs: now,
        collection_eligible_at_secs: now.saturating_add(retention_secs),
    })
}

fn write_metadata(
    quarantine: &Dir,
    name: &str,
    metadata: &IncidentDebrisMetadataV1,
) -> io::Result<()> {
    match quarantine.symlink_metadata(name) {
        Ok(existing) if existing.is_file() => {
            return if read_metadata(quarantine, name)? == *metadata {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "incident debris metadata identity collision",
                ))
            };
        }
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "incident debris metadata is not a regular file",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let bytes = serde_json::to_vec_pretty(metadata)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let temporary = format!(".{}.tmp", metadata.record_id);
    let _ = remove_regular_file_if_exists(quarantine, &temporary);
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No)
        .sync(true);
    #[cfg(unix)]
    options.mode(0o600);
    {
        let mut file = quarantine.open_with(&temporary, &options)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    quarantine.rename(&temporary, quarantine, name)?;
    sync_dir(quarantine)
}

fn read_metadata(quarantine: &Dir, name: &str) -> io::Result<IncidentDebrisMetadataV1> {
    if !is_component(name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "incident debris metadata name is not a component",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let mut file = quarantine.open_with(name, &options)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn metadata_names(quarantine: &Dir, complete: &mut bool) -> Vec<String> {
    let entries = match quarantine.read_dir(".") {
        Ok(entries) => entries,
        Err(_) => {
            *complete = false;
            return Vec::new();
        }
    };
    let mut names = Vec::new();
    for listed in entries {
        let Ok(listed) = listed else {
            *complete = false;
            continue;
        };
        let name = listed.file_name();
        let Some(name) = name.to_str() else {
            *complete = false;
            continue;
        };
        if name.ends_with(".json") {
            if listed.file_type().is_ok_and(|kind| kind.is_file()) {
                names.push(name.to_string());
            } else {
                *complete = false;
            }
        } else if !name.ends_with(".artifact") && !name.ends_with(".tmp") {
            *complete = false;
        }
    }
    names.sort();
    names
}

fn validate_metadata(metadata: &IncidentDebrisMetadataV1, expected_store_id: &str) -> bool {
    metadata.schema == METADATA_SCHEMA_V1
        && metadata.store_id == expected_store_id
        && is_sha256(&metadata.record_id)
        && is_sha256(&metadata.content_sha256)
        && is_component(&metadata.original_name)
        && IncidentDebrisKindV1::classify(&metadata.original_name) == Some(metadata.kind)
        && metadata.collection_eligible_at_secs > metadata.quarantined_at_secs
}

fn verify_artifact(
    quarantine: &Dir,
    name: &str,
    metadata: &IncidentDebrisMetadataV1,
) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let mut file = quarantine.open_with(name, &options)?;
    let observed = file.metadata()?;
    if !observed.is_file() || observed.len() != metadata.size_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "incident debris artifact size mismatch",
        ));
    }
    if sha256_reader(&mut file)? != metadata.content_sha256 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "incident debris artifact digest mismatch",
        ));
    }
    Ok(())
}

fn sha256_reader(mut reader: impl Read) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; HASH_BUFFER_BYTES].into_boxed_slice();
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
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

fn metadata_name(record_id: &str) -> String {
    format!("{record_id}.json")
}

fn artifact_name(record_id: &str) -> String {
    format!("{record_id}.artifact")
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.contains('/')
        && !value.contains('\\')
}

fn remove_regular_file_if_exists(directory: &Dir, name: &str) -> io::Result<()> {
    match directory.symlink_metadata(name) {
        Ok(metadata) if metadata.is_file() => directory.remove_file(name),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "incident debris path is not a regular file",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
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

    use tracedecay_application::storage::IncidentDebrisKindV1;

    use super::*;
    use crate::retention::orphan_stores::{
        StoreCensusEntry, StoreContentFence, StoreDirectoryFence,
    };

    const NOW: i64 = 1_800_000_000;
    const DAY: i64 = 24 * 60 * 60;

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

    fn quarantine_files(store_root: &Path) -> Vec<PathBuf> {
        let quarantine = store_root.join(INCIDENT_DEBRIS_QUARANTINE_DIR);
        let mut files = std::fs::read_dir(quarantine)
            .unwrap()
            .map(|item| item.unwrap().path())
            .collect::<Vec<_>>();
        files.sort();
        files
    }

    #[test]
    fn sweep_quarantines_loose_debris_with_metadata_and_preserves_live_files() {
        let profile = tempfile::tempdir().unwrap();
        let store_root = profile.path().join("stores/store.debris");
        std::fs::create_dir_all(&store_root).unwrap();
        let debris = store_root.join("sessions.db.corrupt-incident");
        let live = store_root.join("sessions.db");
        std::fs::write(&debris, b"debris payload").unwrap();
        std::fs::write(&live, b"live database").unwrap();

        let report = sweep_incident_debris(&[entry(&store_root)], profile.path(), 7 * DAY, NOW);

        assert_eq!(report.quarantined, 1);
        assert_eq!(report.collected, 0);
        assert_eq!(report.retained, 1);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(!debris.exists(), "loose debris must move into quarantine");
        assert!(live.exists(), "live store files must remain untouched");
        let files = quarantine_files(&store_root);
        assert_eq!(files.len(), 2, "artifact plus durable metadata");
        let metadata_path = files
            .iter()
            .find(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .unwrap();
        let metadata: IncidentDebrisMetadataV1 =
            serde_json::from_slice(&std::fs::read(metadata_path).unwrap()).unwrap();
        assert_eq!(metadata.store_id, "store.debris");
        assert_eq!(metadata.original_name, "sessions.db.corrupt-incident");
        assert_eq!(metadata.kind, IncidentDebrisKindV1::Corrupt);
        assert_eq!(metadata.size_bytes, b"debris payload".len() as u64);
        assert_eq!(metadata.quarantined_at_secs, NOW);
        assert_eq!(metadata.collection_eligible_at_secs, NOW + 7 * DAY);

        let scan = scan_incident_debris(&entry(&store_root), profile.path(), NOW).unwrap();
        assert!(scan.listing_complete);
        assert_eq!(scan.artifact_count(), 1);
        assert_eq!(
            scan.artifacts[0].path.as_str(),
            "sessions.db.corrupt-incident"
        );
    }

    #[test]
    fn ordinary_store_subdirectories_do_not_make_scan_partial() {
        let profile = tempfile::tempdir().unwrap();
        let store_root = profile.path().join("stores/store.debris");
        std::fs::create_dir_all(store_root.join("branches")).unwrap();
        std::fs::create_dir_all(store_root.join("payloads")).unwrap();
        std::fs::write(store_root.join("sessions.db"), b"live database").unwrap();

        let scan = scan_incident_debris(&entry(&store_root), profile.path(), NOW).unwrap();

        assert!(scan.listing_complete);
        assert!(scan.artifacts.is_empty());
    }

    #[test]
    fn sweep_collects_only_after_the_quarantine_window() {
        let profile = tempfile::tempdir().unwrap();
        let store_root = profile.path().join("stores/store.debris");
        std::fs::create_dir_all(&store_root).unwrap();
        std::fs::write(
            store_root.join("graph.db.recovered-incident"),
            b"recoverable debris",
        )
        .unwrap();
        let census = [entry(&store_root)];

        let first = sweep_incident_debris(&census, profile.path(), 7 * DAY, NOW);
        assert_eq!(first.quarantined, 1);
        assert_eq!(first.retained, 1);
        let early = sweep_incident_debris(&census, profile.path(), 7 * DAY, NOW + 7 * DAY - 1);
        assert_eq!(early.collected, 0);
        assert_eq!(early.retained, 1);
        assert_eq!(quarantine_files(&store_root).len(), 2);

        let due = sweep_incident_debris(&census, profile.path(), 7 * DAY, NOW + 7 * DAY);
        assert_eq!(due.collected, 1);
        assert_eq!(due.reclaimed_bytes, b"recoverable debris".len() as u64);
        assert_eq!(due.retained, 0);
        assert!(due.errors.is_empty(), "{:?}", due.errors);
        assert!(quarantine_files(&store_root).is_empty());
    }

    #[test]
    fn sweep_rejects_store_capability_outside_the_owner_profile() {
        let profile = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let debris = outside.path().join("sessions.db.corrupt-outside");
        std::fs::write(&debris, b"outside").unwrap();

        let report = sweep_incident_debris(&[entry(outside.path())], profile.path(), 7 * DAY, NOW);

        assert_eq!(report.quarantined, 0);
        assert_eq!(
            report.errors,
            vec![IncidentDebrisFailure {
                store_id: "store.debris".to_string(),
                kind: IncidentDebrisFailureKind::OutsideProfile,
            }]
        );
        assert!(debris.exists());
    }

    #[test]
    fn collection_refuses_tampered_quarantine_content() {
        let profile = tempfile::tempdir().unwrap();
        let store_root = profile.path().join("stores/store.debris");
        std::fs::create_dir_all(&store_root).unwrap();
        std::fs::write(
            store_root.join("recovery-scratch-incident"),
            b"original debris",
        )
        .unwrap();
        let census = [entry(&store_root)];
        let first = sweep_incident_debris(&census, profile.path(), DAY, NOW);
        assert!(first.errors.is_empty());
        let artifact = quarantine_files(&store_root)
            .into_iter()
            .find(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "artifact")
            })
            .unwrap();
        std::fs::write(&artifact, b"tampered").unwrap();

        let due = sweep_incident_debris(&census, profile.path(), DAY, NOW + DAY);

        assert_eq!(due.collected, 0);
        assert_eq!(due.retained, 1);
        assert_eq!(
            due.errors,
            vec![IncidentDebrisFailure {
                store_id: "store.debris".to_string(),
                kind: IncidentDebrisFailureKind::IntegrityMismatch,
            }]
        );
        assert!(artifact.exists(), "tampered evidence must fail closed");
    }
}
