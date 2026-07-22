use super::fixture::load_workspace_snapshot;
use crate::module_scanner::normalize_relative;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManifestClassification {
    FirstParty,
    Fixture,
    Tooling,
    Vendor,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PhysicalManifestLayout {
    pub(super) manifests: BTreeSet<PathBuf>,
    pub(crate) symlinked_rust_sources: BTreeSet<PathBuf>,
    pub(crate) violations: BTreeSet<String>,
}

pub(crate) fn physical_manifest_layout(
    repository: &Path,
) -> Result<PhysicalManifestLayout, String> {
    let tracked = tracked_paths_with_required_manifests(repository)?;
    let live_tracked: Vec<_> = tracked
        .into_iter()
        .filter(|path| fs::symlink_metadata(repository.join(path)).is_ok())
        .collect();
    inspect_physical_manifest_paths(repository, &live_tracked)
}

pub(crate) fn git_tracked_paths(repository: &Path) -> Result<Vec<PathBuf>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(["ls-files", "-z"])
        .output()
        .map_err(|error| {
            format!("cannot list tracked paths for Cargo manifest contract: {error}")
        })?;
    if !output.status.success() {
        return Err(format!(
            "git ls-files failed while discovering Cargo manifests: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|bytes| !bytes.is_empty())
        .map(|bytes| {
            std::str::from_utf8(bytes)
                .map(PathBuf::from)
                .map_err(|error| format!("git-tracked path is not UTF-8: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()
}

pub(super) fn workspace_manifest_paths() -> Result<BTreeSet<PathBuf>, String> {
    Ok(load_workspace_snapshot()?
        .packages
        .into_iter()
        .map(|package| PathBuf::from(package.manifest))
        .collect())
}

pub(super) fn tracked_paths_with_required_manifests(
    repository: &Path,
) -> Result<Vec<PathBuf>, String> {
    let mut paths = git_tracked_paths(repository)?;
    for required in workspace_manifest_paths()? {
        if repository.join(&required).is_file() && !paths.contains(&required) {
            paths.push(required);
        }
    }
    Ok(paths)
}

pub(crate) fn inspect_physical_manifest_paths(
    repository: &Path,
    tracked_paths: &[PathBuf],
) -> Result<PhysicalManifestLayout, String> {
    let canonical_repository = fs::canonicalize(repository)
        .map_err(|error| format!("cannot canonicalize {}: {error}", repository.display()))?;
    let mut candidates = BTreeSet::new();
    let mut symlinked_rust_sources = BTreeSet::new();
    let mut violations = BTreeSet::new();
    for tracked in tracked_paths {
        if tracked.file_name() == Some(OsStr::new("Cargo.toml")) {
            candidates.insert(normalize_relative(tracked)?);
        }
        let absolute = repository.join(tracked);
        let metadata = match fs::symlink_metadata(&absolute) {
            Ok(metadata) => metadata,
            Err(error) => {
                violations.insert(format!(
                    "cannot inspect tracked path {}: {error}",
                    tracked.display()
                ));
                continue;
            }
        };
        if !metadata.file_type().is_symlink() {
            continue;
        }
        let canonical = fs::canonicalize(&absolute).map_err(|error| {
            format!(
                "cannot resolve tracked symlink {}: {error}",
                tracked.display()
            )
        })?;
        if !canonical.starts_with(&canonical_repository) {
            violations.insert(format!(
                "tracked symlink {} resolves outside the repository to {}",
                tracked.display(),
                canonical.display()
            ));
            continue;
        }
        if canonical.is_dir() && canonical.join("Cargo.toml").is_file() {
            candidates.insert(normalize_relative(&tracked.join("Cargo.toml"))?);
        } else if canonical.file_name() == Some(OsStr::new("Cargo.toml")) {
            candidates.insert(normalize_relative(tracked)?);
        }
        if canonical.is_file()
            && (tracked.extension() == Some(OsStr::new("rs"))
                || canonical.extension() == Some(OsStr::new("rs")))
        {
            symlinked_rust_sources.insert(normalize_relative(tracked)?);
        } else if canonical.is_dir() {
            collect_symlinked_rust_sources(
                &canonical_repository,
                &canonical,
                tracked,
                &mut symlinked_rust_sources,
                &mut violations,
            )?;
        }
    }

    let expected = workspace_manifest_paths()?;
    let mut manifests = BTreeSet::new();
    let mut canonical_owners = BTreeMap::<PathBuf, PathBuf>::new();
    for logical in candidates {
        if manifest_classification(&logical) != ManifestClassification::FirstParty {
            continue;
        }
        manifests.insert(logical.clone());
        let absolute = repository.join(&logical);
        let canonical = match fs::canonicalize(&absolute) {
            Ok(canonical) => canonical,
            Err(error) => {
                violations.insert(format!(
                    "cannot canonicalize tracked first-party manifest {}: {error}",
                    logical.display()
                ));
                continue;
            }
        };
        if !canonical.starts_with(&canonical_repository) {
            violations.insert(format!(
                "tracked first-party manifest {} resolves outside the repository to {}",
                logical.display(),
                canonical.display()
            ));
            continue;
        }
        if let Some(other) = canonical_owners.insert(canonical.clone(), logical.clone())
            && other != logical
        {
            violations.insert(format!(
                "tracked manifest symlink aliases the same physical crate: {} and {} -> {}",
                other.display(),
                logical.display(),
                canonical.display()
            ));
        }
        if !expected.contains(&logical) {
            violations.insert(format!(
                "additional tracked first-party Cargo package is forbidden by the workspace contract: {} ({})",
                logical.display(),
                physical_manifest_description(&absolute)?
            ));
        }
    }
    for missing in expected.difference(&manifests) {
        violations.insert(format!(
            "required tracked first-party Cargo manifest is missing: {}",
            missing.display()
        ));
    }
    Ok(PhysicalManifestLayout {
        manifests,
        symlinked_rust_sources,
        violations,
    })
}

fn collect_symlinked_rust_sources(
    canonical_repository: &Path,
    physical_root: &Path,
    logical_root: &Path,
    sources: &mut BTreeSet<PathBuf>,
    violations: &mut BTreeSet<String>,
) -> Result<(), String> {
    let mut pending = VecDeque::from([(physical_root.to_path_buf(), logical_root.to_path_buf())]);
    let mut visited = BTreeSet::new();
    while let Some((physical, logical)) = pending.pop_front() {
        let canonical_directory = fs::canonicalize(&physical)
            .map_err(|error| format!("cannot canonicalize {}: {error}", physical.display()))?;
        if !visited.insert(canonical_directory.clone()) {
            continue;
        }
        for entry in fs::read_dir(&canonical_directory)
            .map_err(|error| format!("cannot read {}: {error}", canonical_directory.display()))?
        {
            let entry = entry.map_err(|error| {
                format!(
                    "cannot read entry in {}: {error}",
                    canonical_directory.display()
                )
            })?;
            let canonical = fs::canonicalize(entry.path())
                .map_err(|error| format!("cannot resolve {}: {error}", entry.path().display()))?;
            let logical = logical.join(entry.file_name());
            if !canonical.starts_with(canonical_repository) {
                violations.insert(format!(
                    "tracked symlink descendant {} resolves outside the repository to {}",
                    logical.display(),
                    canonical.display()
                ));
            } else if canonical.is_dir() {
                pending.push_back((canonical, logical));
            } else if canonical.is_file() && canonical.extension() == Some(OsStr::new("rs")) {
                sources.insert(normalize_relative(&logical)?);
            }
        }
    }
    Ok(())
}

fn manifest_classification(path: &Path) -> ManifestClassification {
    let components: Vec<_> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect();
    if components.first() == Some(&"vendor") {
        ManifestClassification::Vendor
    } else if components.starts_with(&["tests", "fixtures"])
        || components.starts_with(&["eval", "hermetic", "fixtures"])
        || components.starts_with(&["evals", "agent_adoption", "fixture"])
    {
        ManifestClassification::Fixture
    } else if components
        .first()
        .is_some_and(|root| matches!(*root, ".git" | ".worktrees" | "target" | "node_modules"))
    {
        ManifestClassification::Tooling
    } else {
        ManifestClassification::FirstParty
    }
}

fn physical_manifest_description(manifest_path: &Path) -> Result<String, String> {
    let source = fs::read_to_string(manifest_path)
        .map_err(|error| format!("cannot read {}: {error}", manifest_path.display()))?;
    let manifest: toml::Table = toml::from_str(&source)
        .map_err(|error| format!("cannot parse {}: {error}", manifest_path.display()))?;
    let package_name = manifest
        .get("package")
        .and_then(toml::Value::as_table)
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str)
        .unwrap_or("<virtual>");
    let mut targets = Vec::new();
    if let Some(lib) = manifest.get("lib").and_then(toml::Value::as_table) {
        targets.push(format!(
            "lib {} at {}",
            lib.get("name")
                .and_then(toml::Value::as_str)
                .unwrap_or(package_name),
            lib.get("path")
                .and_then(toml::Value::as_str)
                .unwrap_or("src/lib.rs")
        ));
    }
    for (kind, key) in [("bin", "bin"), ("bench", "bench")] {
        if let Some(entries) = manifest.get(key).and_then(toml::Value::as_array) {
            for entry in entries.iter().filter_map(toml::Value::as_table) {
                targets.push(format!(
                    "{kind} {} at {}",
                    entry
                        .get("name")
                        .and_then(toml::Value::as_str)
                        .unwrap_or("<default>"),
                    entry
                        .get("path")
                        .and_then(toml::Value::as_str)
                        .unwrap_or("<default>")
                ));
            }
        }
    }
    Ok(if targets.is_empty() {
        format!("package {package_name}; default targets")
    } else {
        format!("package {package_name}; {}", targets.join(", "))
    })
}
