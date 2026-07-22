use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

const FIXTURE_JSON: &str =
    include_str!("../../fixtures/architecture/workspace_manifest_snapshot.json");
pub(super) const FIXTURE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureDocument {
    schema_version: u32,
    snapshot_sha256: String,
    snapshot: WorkspaceSnapshot,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WorkspaceSnapshot {
    pub(super) packages: Vec<PackageSnapshot>,
    pub(super) root_package_aliases: Vec<PackageAliasSnapshot>,
    pub(super) targets: Vec<TargetSnapshot>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PackageSnapshot {
    pub(super) manifest: String,
    pub(super) package: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) exact_dependencies: Option<Vec<DependencySnapshot>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct PackageAliasSnapshot(pub(super) String, pub(super) String);

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct DependencySnapshot(pub(super) String, pub(super) DependencyKind);

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct TargetSnapshot(
    pub(super) String,
    pub(super) String,
    pub(super) TargetKind,
    pub(super) String,
);

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub(super) enum DependencyKind {
    Normal,
    Dev,
    Build,
}

impl DependencyKind {
    pub(super) fn from_metadata(value: Option<&str>) -> Result<Self, String> {
        match value.unwrap_or("normal") {
            "normal" => Ok(Self::Normal),
            "dev" => Ok(Self::Dev),
            "build" => Ok(Self::Build),
            kind => Err(format!("unknown Cargo dependency kind {kind}")),
        }
    }

    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Dev => "dev",
            Self::Build => "build",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub(super) enum TargetKind {
    Lib,
    Bin,
    Test,
    Example,
    Bench,
    CustomBuild,
}

impl TargetKind {
    pub(super) fn from_metadata(value: &str) -> Result<Self, String> {
        match value {
            "lib" => Ok(Self::Lib),
            "bin" => Ok(Self::Bin),
            "test" => Ok(Self::Test),
            "example" => Ok(Self::Example),
            "bench" => Ok(Self::Bench),
            "custom-build" => Ok(Self::CustomBuild),
            kind => Err(format!("unknown Cargo target kind {kind}")),
        }
    }

    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Lib => "lib",
            Self::Bin => "bin",
            Self::Test => "test",
            Self::Example => "example",
            Self::Bench => "bench",
            Self::CustomBuild => "custom-build",
        }
    }
}

impl TargetSnapshot {
    pub(super) fn display(&self) -> String {
        format!("{}|{}|{}|{}", self.0, self.1, self.2.as_str(), self.3)
    }
}

pub(super) fn load_workspace_snapshot() -> Result<WorkspaceSnapshot, String> {
    let document: FixtureDocument = serde_json::from_str(FIXTURE_JSON)
        .map_err(|error| format!("cannot parse workspace manifest fixture: {error}"))?;
    if document.schema_version != FIXTURE_SCHEMA_VERSION {
        return Err(format!(
            "workspace manifest fixture schema version must be {FIXTURE_SCHEMA_VERSION}, found {}",
            document.schema_version
        ));
    }
    let actual_hash = snapshot_hash(&document.snapshot)?;
    if document.snapshot_sha256 != actual_hash {
        return Err(format!(
            "workspace manifest fixture hash mismatch: expected {}, computed {actual_hash}",
            document.snapshot_sha256
        ));
    }
    validate_snapshot_structure(&document.snapshot)?;
    Ok(document.snapshot)
}

pub(super) fn snapshot_hash(snapshot: &WorkspaceSnapshot) -> Result<String, String> {
    let encoded = serde_json::to_vec(snapshot)
        .map_err(|error| format!("cannot serialize workspace manifest fixture: {error}"))?;
    Ok(Sha256::digest(encoded)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn validate_snapshot_structure(snapshot: &WorkspaceSnapshot) -> Result<(), String> {
    let mut manifests = BTreeSet::new();
    let mut packages = BTreeSet::new();
    for package in &snapshot.packages {
        if !manifests.insert(&package.manifest) {
            return Err(format!(
                "workspace manifest fixture repeats manifest {}",
                package.manifest
            ));
        }
        if !packages.insert(&package.package) {
            return Err(format!(
                "workspace manifest fixture repeats package {}",
                package.package
            ));
        }
        if let Some(dependencies) = &package.exact_dependencies {
            let unique: BTreeSet<_> = dependencies.iter().collect();
            if unique.len() != dependencies.len() {
                return Err(format!(
                    "workspace manifest fixture repeats a dependency for {}",
                    package.package
                ));
            }
        }
    }
    let targets: BTreeSet<_> = snapshot.targets.iter().collect();
    if targets.len() != snapshot.targets.len() {
        return Err("workspace manifest fixture repeats a Cargo target".to_string());
    }
    let aliases: BTreeSet<_> = snapshot.root_package_aliases.iter().collect();
    if aliases.len() != snapshot.root_package_aliases.len() {
        return Err("workspace manifest fixture repeats a root dependency alias".to_string());
    }
    for target in &snapshot.targets {
        if !packages.contains(&target.0) {
            return Err(format!(
                "workspace manifest fixture target belongs to unknown package {}",
                target.0
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn fixture_document() -> Result<(u32, String, WorkspaceSnapshot), String> {
    let document: FixtureDocument = serde_json::from_str(FIXTURE_JSON)
        .map_err(|error| format!("cannot parse workspace manifest fixture: {error}"))?;
    Ok((
        document.schema_version,
        document.snapshot_sha256,
        document.snapshot,
    ))
}
