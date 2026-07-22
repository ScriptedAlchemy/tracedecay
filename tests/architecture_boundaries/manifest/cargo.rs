use super::fixture::{TargetSnapshot, load_workspace_snapshot};
use super::physical::{inspect_physical_manifest_paths, tracked_paths_with_required_manifests};
use super::policy::{validate_target_policy, validate_workspace_package};
use crate::module_scanner::normalize_relative;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const REPOSITORY_SOURCE_ROOTS: &[&str] = &["src", "tests", "examples", "benches"];

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
    workspace_members: BTreeSet<String>,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    #[serde(default)]
    name: String,
    id: String,
    manifest_path: PathBuf,
    #[serde(default)]
    dependencies: Vec<CargoDependency>,
    targets: Vec<CargoTarget>,
}

#[derive(Debug, Deserialize)]
pub(super) struct CargoDependency {
    pub(super) name: String,
    pub(super) rename: Option<String>,
    pub(super) kind: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct CargoTarget {
    #[serde(default)]
    pub(super) name: String,
    pub(super) src_path: PathBuf,
    #[serde(default)]
    pub(super) kind: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CargoSourceLayout {
    pub(crate) target_roots: BTreeSet<PathBuf>,
    pub(crate) tracked_roots: BTreeSet<PathBuf>,
    pub(super) workspace_manifests: BTreeSet<PathBuf>,
    // Kept for callers outside this module until the query-kernel suite is
    // independently reorganized; all internal names and diagnostics are
    // architecture-contract based.
    pub(crate) pr8_violations: BTreeSet<String>,
}

pub(crate) fn cargo_source_layout(repository: &Path) -> Result<CargoSourceLayout, String> {
    let output = Command::new("cargo")
        .current_dir(repository)
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .output()
        .map_err(|error| format!("cannot run cargo metadata: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    parse_cargo_source_layout(repository, &output.stdout)
}

pub(super) fn parse_cargo_source_layout(
    repository: &Path,
    metadata_json: &[u8],
) -> Result<CargoSourceLayout, String> {
    let snapshot = load_workspace_snapshot()?;
    let CargoMetadata {
        packages,
        workspace_members,
    } = serde_json::from_slice(metadata_json)
        .map_err(|error| format!("cannot parse cargo metadata: {error}"))?;
    let package_ids: BTreeSet<_> = packages.iter().map(|package| package.id.clone()).collect();
    let missing_members: Vec<_> = workspace_members.difference(&package_ids).collect();
    if !missing_members.is_empty() {
        return Err(format!(
            "cargo metadata omitted workspace packages: {missing_members:?}"
        ));
    }

    let mut target_roots = BTreeSet::new();
    let mut tracked_roots: BTreeSet<PathBuf> =
        REPOSITORY_SOURCE_ROOTS.iter().map(PathBuf::from).collect();
    let mut workspace_manifests = BTreeSet::new();
    let mut architecture_violations = BTreeSet::new();
    let mut target_snapshot = BTreeSet::new();

    for package in packages {
        if !workspace_members.contains(&package.id) {
            continue;
        }
        let manifest_path = metadata_path_relative(
            repository,
            &package.manifest_path,
            "workspace package manifest",
        )?;
        workspace_manifests.insert(manifest_path.clone());
        validate_workspace_package(
            &manifest_path,
            &package.name,
            &package.dependencies,
            &snapshot,
            &mut architecture_violations,
        );
        let package_root = manifest_path
            .parent()
            .ok_or_else(|| format!("manifest has no parent: {}", manifest_path.display()))?;
        if !package_root.as_os_str().is_empty() {
            tracked_roots.insert(package_root.to_path_buf());
        }

        for target in package.targets {
            let target_path =
                metadata_path_relative(repository, &target.src_path, "Cargo target source")?;
            let canonical_target_path =
                match canonical_repository_relative(repository, &target.src_path) {
                    Ok(path) => path,
                    Err(error) => {
                        architecture_violations.insert(format!(
                            "{} target {} has invalid source path: {error}",
                            manifest_path.display(),
                            target.name
                        ));
                        target_path.clone()
                    }
                };
            if let Some(kind) = validate_target_policy(
                &manifest_path,
                &package.name,
                &target,
                &canonical_target_path,
                &mut architecture_violations,
            ) {
                target_snapshot.insert(TargetSnapshot(
                    package.name.clone(),
                    target.name.clone(),
                    kind,
                    canonical_target_path.to_string_lossy().into_owned(),
                ));
            }
            target_roots.insert(target_path);
        }
    }

    if target_roots.is_empty() {
        return Err("cargo metadata exposes no workspace Rust targets".to_string());
    }
    for target_root in &target_roots {
        if !tracked_roots
            .iter()
            .any(|source_root| target_root.starts_with(source_root))
        {
            tracked_roots.insert(target_root.clone());
        }
    }

    let expected_manifests: BTreeSet<_> = snapshot
        .packages
        .iter()
        .map(|package| PathBuf::from(&package.manifest))
        .collect();
    for missing in expected_manifests.difference(&workspace_manifests) {
        architecture_violations.insert(format!(
            "required workspace contract member is missing: {}",
            missing.display()
        ));
    }
    for extra in workspace_manifests.difference(&expected_manifests) {
        architecture_violations.insert(format!(
            "additional workspace contract member is forbidden: {}",
            extra.display()
        ));
    }
    let expected_targets: BTreeSet<_> = snapshot.targets.into_iter().collect();
    for missing in expected_targets.difference(&target_snapshot) {
        architecture_violations.insert(format!(
            "required workspace Cargo target is missing: {}",
            missing.display()
        ));
    }
    for extra in target_snapshot.difference(&expected_targets) {
        architecture_violations.insert(format!(
            "additional workspace Cargo target is forbidden: {}",
            extra.display()
        ));
    }

    Ok(CargoSourceLayout {
        target_roots,
        tracked_roots,
        workspace_manifests,
        pr8_violations: architecture_violations,
    })
}

fn canonical_repository_relative(repository: &Path, path: &Path) -> Result<PathBuf, String> {
    let canonical_repository = fs::canonicalize(repository)
        .map_err(|error| format!("cannot canonicalize {}: {error}", repository.display()))?;
    let canonical = fs::canonicalize(path)
        .map_err(|error| format!("cannot canonicalize {}: {error}", path.display()))?;
    let relative = canonical.strip_prefix(&canonical_repository).map_err(|_| {
        format!(
            "{} resolves outside repository to {}",
            path.display(),
            canonical.display()
        )
    })?;
    normalize_relative(relative)
}

fn metadata_path_relative(
    repository: &Path,
    path: &Path,
    description: &str,
) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!(
            "{description} path is not absolute: {}",
            path.display()
        ));
    }
    let relative = path.strip_prefix(repository).map_err(|_| {
        format!(
            "{description} path is outside repository: {}",
            path.display()
        )
    })?;
    normalize_relative(relative)
}

pub(super) fn git_tracked_rust_sources(
    repository: &Path,
    source_roots: &BTreeSet<PathBuf>,
) -> Result<BTreeSet<PathBuf>, String> {
    let tracked = tracked_paths_with_required_manifests(repository)?;
    let live_tracked: Vec<_> = tracked
        .into_iter()
        .filter(|path| fs::symlink_metadata(repository.join(path)).is_ok())
        .collect();
    let physical = inspect_physical_manifest_paths(repository, &live_tracked)?;
    if !physical.violations.is_empty() {
        return Err(format!(
            "tracked path contract violations:\n{}",
            physical
                .violations
                .iter()
                .map(|violation| format!("  - {violation}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    let mut sources = BTreeSet::new();
    for path in live_tracked {
        if path.extension() != Some(OsStr::new("rs"))
            || !source_roots.iter().any(|root| path.starts_with(root))
        {
            continue;
        }
        let canonical = canonical_repository_relative(repository, &repository.join(&path))?;
        if !repository.join(&canonical).is_file() {
            return Err(format!(
                "tracked Rust source does not resolve to a file: {}",
                path.display()
            ));
        }
        sources.insert(normalize_relative(&path)?);
    }
    sources.extend(
        physical
            .symlinked_rust_sources
            .into_iter()
            .filter(|path| source_roots.iter().any(|root| path.starts_with(root))),
    );
    sources.extend(filesystem_rust_sources(repository, source_roots)?);
    Ok(sources)
}

pub(crate) fn filesystem_rust_sources(
    repository: &Path,
    source_roots: &BTreeSet<PathBuf>,
) -> Result<BTreeSet<PathBuf>, String> {
    let mut pending: Vec<_> = source_roots
        .iter()
        .map(|root| repository.join(root))
        .collect();
    let mut sources = BTreeSet::new();
    while let Some(path) = pending.pop() {
        if !path.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&path).map_err(|error| {
            format!("cannot read source directory '{}': {error}", path.display())
        })? {
            let entry = entry.map_err(|error| {
                format!(
                    "cannot read entry in source directory '{}': {error}",
                    path.display()
                )
            })?;
            let file_type = entry.file_type().map_err(|error| {
                format!(
                    "cannot inspect source path '{}': {error}",
                    entry.path().display()
                )
            })?;
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file() && entry.path().extension() == Some(OsStr::new("rs")) {
                let entry_path = entry.path();
                let relative = entry_path.strip_prefix(repository).map_err(|_| {
                    format!(
                        "source path is outside repository: {}",
                        entry_path.display()
                    )
                })?;
                sources.insert(normalize_relative(relative)?);
            }
        }
    }
    Ok(sources)
}
