use super::cargo::{CargoDependency, CargoTarget};
use super::fixture::{DependencyKind, PackageSnapshot, TargetKind, WorkspaceSnapshot};
use crate::module_scanner::normalize_identifier;
use std::collections::BTreeSet;
use std::path::Path;

const QUERY_ALLOWED_PACKAGES: &[&str] = &[
    "hex",
    "hmac",
    "serde",
    "serde_json",
    "sha2",
    "thiserror",
    "tracedecay-domain",
    "tracedecay-policy",
    "tracedecay-store",
    "tracedecay-tool-catalog",
    "zeroize",
];

#[derive(Debug, Clone, Copy)]
enum PackageRole {
    PureQueryContract,
    SqliteParityProtocol,
    RusqliteParityProbe,
    BundledRusqliteRuntime,
}

impl PackageRole {
    fn for_manifest(manifest: &str) -> Self {
        match manifest {
            "crates/tracedecay-sqlite-parity-protocol/Cargo.toml" => Self::SqliteParityProtocol,
            "crates/tracedecay-rusqlite-parity/Cargo.toml" => Self::RusqliteParityProbe,
            "crates/tracedecay-rusqlite-runtime/Cargo.toml" => Self::BundledRusqliteRuntime,
            _ => Self::PureQueryContract,
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::PureQueryContract => "pure query contract",
            Self::SqliteParityProtocol => "driver-free SQLite parity protocol",
            Self::RusqliteParityProbe => "process-isolated rusqlite parity probe",
            Self::BundledRusqliteRuntime => "private bundled rusqlite storage runtime",
        }
    }
}

pub(super) fn validate_workspace_package(
    manifest_path: &Path,
    package_name: &str,
    dependencies: &[CargoDependency],
    snapshot: &WorkspaceSnapshot,
    violations: &mut BTreeSet<String>,
) {
    let manifest = manifest_path.to_string_lossy();
    let Some(expected) = snapshot
        .packages
        .iter()
        .find(|package| package.manifest == manifest)
    else {
        violations.insert(format!(
            "unknown workspace package is forbidden: {} ({package_name})",
            manifest_path.display()
        ));
        return;
    };
    if package_name != expected.package {
        violations.insert(format!(
            "{} must declare workspace contract package name {}, found {package_name}",
            manifest_path.display(),
            expected.package
        ));
    }

    if manifest_path == Path::new("Cargo.toml") {
        validate_root_dependencies(dependencies, snapshot, violations);
    } else {
        validate_contract_dependencies(manifest_path, dependencies, expected, violations);
    }
}

fn validate_root_dependencies(
    dependencies: &[CargoDependency],
    snapshot: &WorkspaceSnapshot,
    violations: &mut BTreeSet<String>,
) {
    let aliases: BTreeSet<_> = dependencies
        .iter()
        .filter_map(|dependency| {
            dependency
                .rename
                .as_ref()
                .map(|alias| (alias.as_str(), dependency.name.as_str()))
        })
        .collect();
    let expected_aliases: BTreeSet<_> = snapshot
        .root_package_aliases
        .iter()
        .map(|alias| (alias.0.as_str(), alias.1.as_str()))
        .collect();
    for missing in expected_aliases.difference(&aliases) {
        violations.insert(format!(
            "required root package dependency alias {} -> {} is missing",
            missing.0, missing.1
        ));
    }
    for extra in aliases.difference(&expected_aliases) {
        violations.insert(format!(
            "root package dependency alias {} -> {} is outside the workspace contract snapshot",
            extra.0, extra.1
        ));
    }

    for dependency in dependencies {
        let alias = dependency.rename.as_deref().unwrap_or(&dependency.name);
        let normalized_alias = normalize_identifier(alias);
        if let Some(expected_package) = allowed_package_for_query_root(&normalized_alias)
            && normalize_identifier(&dependency.name) != normalize_identifier(expected_package)
        {
            violations.insert(format!(
                "dependency alias {alias} maps allowlisted query root {normalized_alias} to non-allowlisted package {}",
                dependency.name
            ));
        }
    }
}

fn validate_contract_dependencies(
    manifest_path: &Path,
    dependencies: &[CargoDependency],
    package: &PackageSnapshot,
    violations: &mut BTreeSet<String>,
) {
    let role = PackageRole::for_manifest(&package.manifest);
    if let Some(expected_dependencies) = &package.exact_dependencies {
        let actual: BTreeSet<_> = dependencies
            .iter()
            .filter_map(|dependency| {
                dependency_kind(dependency, violations).map(|kind| (dependency.name.as_str(), kind))
            })
            .collect();
        let expected: BTreeSet<_> = expected_dependencies
            .iter()
            .map(|dependency| (dependency.0.as_str(), dependency.1))
            .collect();
        for missing in expected.difference(&actual) {
            violations.insert(format!(
                "{} {} dependency is missing: {}|{}",
                manifest_path.display(),
                role.description(),
                missing.0,
                missing.1.as_str()
            ));
        }
        for extra in actual.difference(&expected) {
            violations.insert(format!(
                "{} {} dependency is forbidden: {}|{}",
                manifest_path.display(),
                role.description(),
                extra.0,
                extra.1.as_str()
            ));
        }
        for dependency in dependencies {
            if let Some(rename) = &dependency.rename {
                violations.insert(format!(
                    "{} {} must not rename dependency {rename} -> {}",
                    manifest_path.display(),
                    role.description(),
                    dependency.name
                ));
            }
        }
        return;
    }

    for dependency in dependencies {
        let alias = dependency.rename.as_deref().unwrap_or(&dependency.name);
        let normalized_alias = normalize_identifier(alias);
        let package_allowed = QUERY_ALLOWED_PACKAGES
            .iter()
            .any(|allowed| normalize_identifier(allowed) == normalize_identifier(&dependency.name));
        let alias_matches_package =
            allowed_package_for_query_root(&normalized_alias).is_some_and(|expected| {
                normalize_identifier(expected) == normalize_identifier(&dependency.name)
            });
        if !package_allowed || !alias_matches_package {
            violations.insert(format!(
                "{} contract dependency {alias} -> {} is outside the pure query package allowlist",
                manifest_path.display(),
                dependency.name
            ));
        }
    }
}

fn dependency_kind(
    dependency: &CargoDependency,
    violations: &mut BTreeSet<String>,
) -> Option<DependencyKind> {
    match DependencyKind::from_metadata(dependency.kind.as_deref()) {
        Ok(kind) => Some(kind),
        Err(error) => {
            violations.insert(format!("dependency {} has {error}", dependency.name));
            None
        }
    }
}

fn allowed_package_for_query_root(root: &str) -> Option<&'static str> {
    QUERY_ALLOWED_PACKAGES
        .iter()
        .copied()
        .find(|package| normalize_identifier(package) == root)
}

pub(super) fn validate_target_policy(
    manifest_path: &Path,
    package_name: &str,
    target: &CargoTarget,
    target_path: &Path,
    violations: &mut BTreeSet<String>,
) -> Option<TargetKind> {
    let kind = if target.kind.len() == 1 {
        match TargetKind::from_metadata(&target.kind[0]) {
            Ok(kind) => Some(kind),
            Err(error) => {
                violations.insert(format!(
                    "{} package {package_name} target {} has {error}",
                    manifest_path.display(),
                    target.name
                ));
                None
            }
        }
    } else {
        violations.insert(format!(
            "{} package {package_name} target {} has non-exact kinds {:?}",
            manifest_path.display(),
            target.name,
            target.kind
        ));
        None
    };
    if target_path.starts_with("src/query")
        || target_path.components().any(|component| {
            matches!(component, std::path::Component::Normal(name) if matches!(normalize_identifier(name.to_string_lossy().as_ref()).as_str(), "query" | "kernel"))
        })
    {
        violations.insert(format!(
            "{} package {package_name} exposes query code as {:?} target {} at {}",
            manifest_path.display(),
            target.kind,
            target.name,
            target_path.display()
        ));
    }
    if matches!(
        normalize_identifier(&target.name).as_str(),
        "query" | "query_kernel" | "temporal_query" | "temporal_kernel"
    ) {
        violations.insert(format!(
            "{} package {package_name} exposes reserved query/kernel target name {} ({:?})",
            manifest_path.display(),
            target.name,
            target.kind
        ));
    }
    kind
}
