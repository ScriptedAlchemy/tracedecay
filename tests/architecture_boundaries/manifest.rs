//! Cargo manifest contract guards.
//!
//! Validates driver-neutral workspace dependency and target boundaries,
//! classifies physical Cargo manifests without name heuristics, and enumerates
//! git-tracked Rust sources without freezing an exact package/target snapshot.

use crate::module_scanner::{normalize_identifier, normalize_relative, resolve_reachable_sources};
#[cfg(unix)]
use crate::query_kernel::query_kernel_violations;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsStr;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const REPOSITORY_SOURCE_ROOTS: &[&str] = &["src", "tests", "examples", "benches"];
const QUERY_ALLOWED_PACKAGES: &[&str] = &[
    "hex",
    "hmac",
    "schemars",
    "serde",
    "serde_json",
    "sha2",
    "thiserror",
    "tracedecay-domain",
    "tracedecay-policy",
    "tracedecay-store",
    "tracedecay-tool-catalog",
    "url",
    "zeroize",
];
#[cfg(unix)]
const TEST_WORKSPACE_MANIFESTS: &[&str] = &["Cargo.toml", "crates/tracedecay-domain/Cargo.toml"];
const ALLOWED_ROOT_PACKAGE_ALIASES: &[(&str, &str)] = &[
    (
        "tracedecay-medium-treesitters",
        "tokensave-medium-treesitters",
    ),
    (
        "tracedecay-large-treesitters",
        "tokensave-large-treesitters",
    ),
];
const FORBIDDEN_ROOT_RUNTIME_PACKAGES: &[&str] = &["libsql"];

// This is a sample project indexed by context-evaluation tests. Its Rust files
// are deliberately source input, not modules or targets of the tracedecay crate.
const INTENTIONAL_STANDALONE_RUST_INPUTS: &[&str] = &[
    "tests/fixtures/context_eval_project/src/auth/login.rs",
    "tests/fixtures/context_eval_project/src/auth/mod.rs",
    "tests/fixtures/context_eval_project/src/auth/session.rs",
    "tests/fixtures/context_eval_project/src/cli.rs",
    "tests/fixtures/context_eval_project/src/main.rs",
    "tests/fixtures/context_eval_project/src/net/http_client.rs",
    "tests/fixtures/context_eval_project/src/net/mod.rs",
    "tests/fixtures/context_eval_project/src/net/retry.rs",
    "tests/fixtures/context_eval_project/src/storage/cache.rs",
    "tests/fixtures/context_eval_project/src/storage/config_store.rs",
    "tests/fixtures/context_eval_project/src/storage/mod.rs",
    // Managed-run diagnostics fixture: copied into a temporary project and
    // indexed as source input rather than compiled by this workspace.
    "tests/fixtures/pr12_managed_run_overlay/src/auth/login.rs",
    // Search-quality evaluation corpus: sample project sources indexed by the
    // PR9 search-eval harness. Deliberately source input, not crate modules.
    "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/integration.rs",
    "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/repository.rs",
    "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/canonical.rs",
    "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/coverage.rs",
    "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/error.rs",
    "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/id.rs",
    "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/time.rs",
    "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/watermark.rs",
    "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/session.rs",
    "tests/fixtures/search_quality/incremental/time-after.rs",
    // Distribution acceptance copies this into the packaged example path; it is
    // not a workspace Cargo target in the development tree.
    "tests/distribution/fastembed/acceptance.rs",
];

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
struct CargoDependency {
    name: String,
    rename: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CargoTarget {
    #[serde(default)]
    name: String,
    src_path: PathBuf,
    #[serde(default)]
    kind: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CargoSourceLayout {
    pub(crate) target_roots: BTreeSet<PathBuf>,
    pub(crate) tracked_roots: BTreeSet<PathBuf>,
    workspace_manifests: BTreeSet<PathBuf>,
    pub(crate) boundary_violations: BTreeSet<String>,
}

static WORKSPACE_CARGO_SOURCE_LAYOUT: OnceLock<Result<CargoSourceLayout, String>> = OnceLock::new();

pub(crate) fn cargo_source_layout(repository: &Path) -> Result<CargoSourceLayout, String> {
    if repository == Path::new(env!("CARGO_MANIFEST_DIR")) {
        return WORKSPACE_CARGO_SOURCE_LAYOUT
            .get_or_init(|| discover_cargo_source_layout(repository))
            .clone();
    }
    discover_cargo_source_layout(repository)
}

fn discover_cargo_source_layout(repository: &Path) -> Result<CargoSourceLayout, String> {
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

fn parse_cargo_source_layout(
    repository: &Path,
    metadata_json: &[u8],
) -> Result<CargoSourceLayout, String> {
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
    let mut boundary_violations = BTreeSet::new();

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
        let package_root = manifest_path
            .parent()
            .ok_or_else(|| format!("manifest has no parent: {}", manifest_path.display()))?;
        if !package_root.as_os_str().is_empty() {
            tracked_roots.insert(package_root.to_path_buf());
        }

        if manifest_path == Path::new("Cargo.toml") {
            validate_query_dependency_aliases(&package.dependencies, &mut boundary_violations);
        } else {
            if !manifest_path.starts_with("crates") {
                boundary_violations.insert(format!(
                    "workspace member {} manifest {} is outside the approved crates/ layout",
                    package.name,
                    manifest_path.display()
                ));
            }
            validate_contract_package_dependencies(
                &manifest_path,
                &package.dependencies,
                &mut boundary_violations,
            );
        }

        for target in package.targets {
            let target_path =
                metadata_path_relative(repository, &target.src_path, "Cargo target source")?;
            let canonical_target_path =
                match canonical_repository_relative(repository, &target.src_path) {
                    Ok(path) => path,
                    Err(error) => {
                        boundary_violations.insert(format!(
                            "{} target {} has invalid source path: {error}",
                            manifest_path.display(),
                            target.name
                        ));
                        target_path.clone()
                    }
                };
            validate_target_boundary(
                &manifest_path,
                &package.name,
                &target,
                &canonical_target_path,
                &mut boundary_violations,
            );
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

    Ok(CargoSourceLayout {
        target_roots,
        tracked_roots,
        workspace_manifests,
        boundary_violations,
    })
}

fn validate_query_dependency_aliases(
    dependencies: &[CargoDependency],
    violations: &mut BTreeSet<String>,
) {
    for dependency in dependencies {
        let alias = dependency.rename.as_deref().unwrap_or(&dependency.name);
        if let Some(rename) = &dependency.rename
            && !ALLOWED_ROOT_PACKAGE_ALIASES
                .iter()
                .any(|(allowed_alias, package)| {
                    rename.as_str() == *allowed_alias && dependency.name.as_str() == *package
                })
        {
            violations.insert(format!(
                "root package dependency alias {rename} -> {} is not in the approved dependency alias set",
                dependency.name
            ));
        }
        let normalized_alias = normalize_identifier(alias);
        let Some(expected_package) = allowed_package_for_query_root(&normalized_alias) else {
            continue;
        };
        if normalize_identifier(&dependency.name) != normalize_identifier(expected_package) {
            violations.insert(format!(
                "dependency alias {alias} maps allowlisted query root {normalized_alias} to non-allowlisted package {}",
                dependency.name
            ));
        }
    }
}

fn forbidden_root_runtime_dependencies(
    manifest: &toml::Table,
    forbidden_packages: &[&str],
) -> BTreeSet<String> {
    let mut violations = BTreeSet::new();
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        inspect_dependency_table(
            manifest.get(section),
            section,
            forbidden_packages,
            &mut violations,
        );
    }
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        for (selector, target) in targets {
            let Some(target) = target.as_table() else {
                continue;
            };
            for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
                inspect_dependency_table(
                    target.get(section),
                    &format!("target.{selector}.{section}"),
                    forbidden_packages,
                    &mut violations,
                );
            }
        }
    }
    violations
}

fn inspect_dependency_table(
    value: Option<&toml::Value>,
    section: &str,
    forbidden_packages: &[&str],
    violations: &mut BTreeSet<String>,
) {
    let Some(dependencies) = value.and_then(toml::Value::as_table) else {
        return;
    };
    for (alias, specification) in dependencies {
        let package = specification
            .as_table()
            .and_then(|table| table.get("package"))
            .and_then(toml::Value::as_str)
            .unwrap_or(alias);
        if forbidden_packages
            .iter()
            .any(|forbidden| package == *forbidden)
        {
            violations.insert(format!(
                "root Cargo manifest {section} aliases forbidden runtime package {alias} -> {package}"
            ));
        }
    }
}

fn validate_contract_package_dependencies(
    manifest_path: &Path,
    dependencies: &[CargoDependency],
    violations: &mut BTreeSet<String>,
) {
    let allowed_packages = contract_allowed_packages(manifest_path);
    for dependency in dependencies {
        let alias = dependency.rename.as_deref().unwrap_or(&dependency.name);
        let normalized_alias = normalize_identifier(alias);
        let package_allowed = allowed_packages
            .iter()
            .any(|allowed| normalize_identifier(allowed) == normalize_identifier(&dependency.name));
        let alias_matches_package = allowed_packages.iter().any(|allowed| {
            normalize_identifier(allowed) == normalized_alias
                && normalize_identifier(allowed) == normalize_identifier(&dependency.name)
        });
        if !package_allowed || !alias_matches_package {
            violations.insert(format!(
                "{} contract dependency {alias} -> {} is outside the package allowlist",
                manifest_path.display(),
                dependency.name
            ));
        }
    }
}

#[test]
fn root_manifest_rejects_libsql_runtime_dependencies() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest: toml::Table = fs::read_to_string(repository.join("Cargo.toml"))
        .expect("read root Cargo manifest")
        .parse()
        .expect("parse root Cargo manifest");
    assert!(
        forbidden_root_runtime_dependencies(&manifest, FORBIDDEN_ROOT_RUNTIME_PACKAGES).is_empty(),
        "root Cargo manifest must not restore libsql dependencies"
    );

    for fixture in [
        "[dependencies]\nlibsql = \"0.9\"\n",
        "[dev-dependencies]\nlibsql = \"0.9\"\n",
        "[build-dependencies]\nsqlite-driver = { package = \"libsql\", version = \"0.9\" }\n",
        "[target.'cfg(windows)'.dependencies]\nlibsql = \"0.9\"\n",
        "[target.'cfg(windows)'.dev-dependencies]\nsqlite-driver = { package = \"libsql\", version = \"0.9\" }\n",
        "[target.'cfg(unix)'.build-dependencies]\nsqlite-driver = { package = \"libsql\", version = \"0.9\" }\n",
    ] {
        let manifest: toml::Table = fixture.parse().expect("parse forbidden manifest fixture");
        assert!(
            !forbidden_root_runtime_dependencies(&manifest, FORBIDDEN_ROOT_RUNTIME_PACKAGES)
                .is_empty(),
            "root manifest guard accepted forbidden dependency fixture: {fixture}"
        );
    }

    let allowed: toml::Table = "[dependencies]\nlibsqlite3-sys = \"0.38\"\n"
        .parse()
        .expect("parse allowed manifest fixture");
    assert!(
        forbidden_root_runtime_dependencies(&allowed, FORBIDDEN_ROOT_RUNTIME_PACKAGES).is_empty()
    );
}

fn contract_allowed_packages(manifest_path: &Path) -> &'static [&'static str] {
    match manifest_path.to_str() {
        Some("crates/tracedecay-code-index/Cargo.toml") => &[
            "ast-grep-core",
            "cc",
            "ignore",
            "serde",
            "serde_json",
            "sha2",
            "static_assertions",
            "tempfile",
            "thiserror",
            "tokensave-large-treesitters",
            "tokensave-medium-treesitters",
            "tracedecay-application",
            "tracedecay-domain",
            "tree-sitter",
            "tree-sitter-hlsl",
            "tree-sitter-language",
        ],
        Some("crates/tracedecay-api/Cargo.toml") => &[
            "axum",
            "futures-util",
            "serde",
            "serde_json",
            "thiserror",
            "tracedecay-application",
            "tracedecay-tool-catalog",
        ],
        Some("crates/tracedecay-hooks/Cargo.toml") => &[
            "serde",
            "serde_json",
            "thiserror",
            "tracedecay-application",
            "tracedecay-domain",
            "tracedecay-tool-catalog",
        ],
        Some("crates/tracedecay-rusqlite-parity/Cargo.toml") => &[
            "hex",
            "rusqlite",
            "serde_json",
            "sha2",
            "tempfile",
            "tracedecay-sqlite-parity-protocol",
            "url",
        ],
        Some("crates/tracedecay-rusqlite-runtime/Cargo.toml") => &[
            "proptest",
            "rusqlite",
            "serde",
            "serde_json",
            "sha2",
            "tempfile",
            "tokio",
            "tracedecay-application",
            "tracedecay-domain",
            "tracedecay-store",
            "tracedecay-tool-catalog",
        ],
        Some("crates/tracedecay-sqlite-parity-protocol/Cargo.toml") => {
            &["hex", "serde", "serde_json", "sha2", "tempfile"]
        }
        _ => QUERY_ALLOWED_PACKAGES,
    }
}

fn allowed_package_for_query_root(root: &str) -> Option<&'static str> {
    QUERY_ALLOWED_PACKAGES
        .iter()
        .copied()
        .find(|package| normalize_identifier(package) == root)
}

fn validate_target_boundary(
    manifest_path: &Path,
    package_name: &str,
    target: &CargoTarget,
    target_path: &Path,
    violations: &mut BTreeSet<String>,
) {
    if target.kind.len() != 1 {
        violations.insert(format!(
            "{} package {package_name} target {} has ambiguous target kinds {:?}",
            manifest_path.display(),
            target.name,
            target.kind
        ));
    }
    if target_path.starts_with("src/query")
        || target_path
            .components()
            .any(|component| matches!(component, Component::Normal(name) if matches!(normalize_identifier(name.to_string_lossy().as_ref()).as_str(), "query" | "kernel")))
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

fn git_tracked_rust_sources(
    repository: &Path,
    source_roots: &BTreeSet<PathBuf>,
) -> Result<BTreeSet<PathBuf>, String> {
    let tracked = tracked_paths_with_required_manifests(repository)?;
    // Validate the live worktree rather than assuming the index and filesystem
    // are identical. During a normal unstaged module move, `git ls-files`
    // still names the deleted source while the replacement module is
    // intentionally untracked. Missing index entries are excluded here and
    // the filesystem walk below adds their live replacements.
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
            || !is_within_source_roots(&path, source_roots)
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
            .filter(|path| is_within_source_roots(path, source_roots)),
    );
    sources.extend(filesystem_rust_sources(repository, source_roots)?);
    Ok(sources)
}

fn is_within_source_roots(path: &Path, source_roots: &BTreeSet<PathBuf>) -> bool {
    path.ancestors()
        .any(|ancestor| source_roots.contains(ancestor))
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
        if path.is_dir() {
            let entries = fs::read_dir(&path).map_err(|error| {
                format!("cannot read source directory '{}': {error}", path.display())
            })?;
            for entry in entries {
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
                } else if file_type.is_file() && entry.path().extension() == Some(OsStr::new("rs"))
                {
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
    }
    Ok(sources)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManifestClassification {
    FirstParty,
    Fixture,
    Tooling,
    Vendor,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PhysicalManifestLayout {
    manifests: BTreeSet<PathBuf>,
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

fn tracked_paths_with_required_manifests(repository: &Path) -> Result<Vec<PathBuf>, String> {
    git_tracked_paths(repository)
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

    let mut manifests = BTreeSet::new();
    let mut canonical_owners = BTreeMap::<PathBuf, PathBuf>::new();
    for logical in candidates {
        if manifest_classification(&logical) != ManifestClassification::FirstParty {
            continue;
        }
        if logical != Path::new("Cargo.toml") && !logical.starts_with("crates") {
            violations.insert(out_of_layout_manifest_violation(repository, &logical));
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

/// Describes a first-party Cargo manifest tracked outside the approved
/// `Cargo.toml`/`crates/` layout. The message quotes the physical package and
/// library identity read from the manifest so the contract cannot be satisfied
/// by renaming the package or its target.
fn out_of_layout_manifest_violation(repository: &Path, logical: &Path) -> String {
    let package = manifest_declared_name(repository, logical, "package")
        .unwrap_or_else(|| "<unknown>".to_string());
    let library =
        manifest_declared_name(repository, logical, "lib").unwrap_or_else(|| package.clone());
    format!(
        "first-party manifest {} declares package {} (lib {}) outside the approved crates/ layout",
        logical.display(),
        package,
        library
    )
}

/// Reads the `name` field from the `[package]` or `[lib]` table of a manifest.
fn manifest_declared_name(repository: &Path, logical: &Path, table: &str) -> Option<String> {
    let contents = fs::read_to_string(repository.join(logical)).ok()?;
    let document: toml::Table = contents.parse().ok()?;
    document
        .get(table)?
        .get("name")?
        .as_str()
        .map(str::to_string)
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

#[test]
fn git_tracked_rust_sources_are_reachable_from_cargo_targets() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let layout = cargo_source_layout(repository).expect("discover Cargo workspace Rust targets");
    let reachable = resolve_reachable_sources(repository, &layout.target_roots)
        .expect("resolve Rust module/include graph");
    let tracked = git_tracked_rust_sources(repository, &layout.tracked_roots)
        .expect("list git-tracked workspace Rust sources");
    let allowlisted: BTreeSet<PathBuf> = INTENTIONAL_STANDALONE_RUST_INPUTS
        .iter()
        .map(|path| PathBuf::from(*path))
        .collect();
    let stale_allowlist: Vec<_> = allowlisted.difference(&tracked).collect();
    assert!(
        stale_allowlist.is_empty(),
        "standalone Rust input allowlist contains untracked or deleted paths: {stale_allowlist:?}"
    );
    let reachable_allowlist: Vec<_> = allowlisted.intersection(&reachable).collect();
    assert!(
        reachable_allowlist.is_empty(),
        "Rust inputs are now reachable and should leave the standalone allowlist: {reachable_allowlist:?}"
    );
    let orphaned: Vec<_> = tracked
        .difference(&reachable)
        .filter(|path| !allowlisted.contains(*path))
        .collect();

    assert!(
        orphaned.is_empty(),
        "git-tracked Rust files are not reachable from any Cargo target:\n{}\n\
         Register each file from a target/module root, or document a genuinely standalone source \
         input in INTENTIONAL_STANDALONE_RUST_INPUTS.",
        orphaned
            .iter()
            .map(|path| format!("  - {}", path.display()))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn metadata_layout_includes_workspace_targets_and_scopes_tracked_sources() {
    let temporary = tempfile::tempdir().expect("create metadata fixture");
    let repository = temporary.path();
    let root_id = "path+file:///workspace#root@0.1.0";
    let api_id = "path+file:///workspace/crates/tracedecay-api#tracedecay-api@0.1.0";
    let application_id =
        "path+file:///workspace/crates/tracedecay-application#tracedecay-application@0.1.0";
    let domain_id = "path+file:///workspace/crates/domain#domain@0.1.0";
    let hooks_id = "path+file:///workspace/crates/tracedecay-hooks#tracedecay-hooks@0.1.0";
    let policy_id = "path+file:///workspace/crates/tracedecay-policy#tracedecay-policy@0.1.0";
    let parity_id =
        "path+file:///workspace/crates/tracedecay-rusqlite-parity#tracedecay-rusqlite-parity@0.1.0";
    let runtime_id = "path+file:///workspace/crates/tracedecay-rusqlite-runtime#tracedecay-rusqlite-runtime@0.1.0";
    let protocol_id = "path+file:///workspace/crates/tracedecay-sqlite-parity-protocol#tracedecay-sqlite-parity-protocol@0.1.0";
    let store_id = "path+file:///workspace/crates/store#store@0.1.0";
    let catalog_id = "path+file:///workspace/crates/tool-catalog#tool-catalog@0.1.0";
    let metadata = serde_json::json!({
        "packages": [
            {
                "id": root_id,
                "name": "tracedecay",
                "manifest_path": repository.join("Cargo.toml"),
                "targets": [
                    { "src_path": repository.join("src/lib.rs") },
                    { "src_path": repository.join("src/main.rs") },
                    { "src_path": repository.join("build.rs") }
                ]
            },
            {
                "id": api_id,
                "name": "tracedecay-api",
                "manifest_path": repository.join("crates/tracedecay-api/Cargo.toml"),
                "targets": [
                    { "src_path": repository.join("crates/tracedecay-api/src/lib.rs") }
                ]
            },
            {
                "id": application_id,
                "name": "tracedecay-application",
                "manifest_path": repository.join("crates/tracedecay-application/Cargo.toml"),
                "targets": [
                    { "src_path": repository.join("crates/tracedecay-application/src/lib.rs") }
                ]
            },
            {
                "id": hooks_id,
                "name": "tracedecay-hooks",
                "manifest_path": repository.join("crates/tracedecay-hooks/Cargo.toml"),
                "targets": [
                    { "src_path": repository.join("crates/tracedecay-hooks/src/lib.rs") }
                ]
            },
            {
                "id": domain_id,
                "name": "tracedecay-domain",
                "manifest_path": repository.join("crates/tracedecay-domain/Cargo.toml"),
                "targets": [
                    { "src_path": repository.join("crates/tracedecay-domain/src/lib.rs") },
                    { "src_path": repository.join("crates/tracedecay-domain/tests/boundary.rs") }
                ]
            },
            {
                "id": policy_id,
                "name": "tracedecay-policy",
                "manifest_path": repository.join("crates/tracedecay-policy/Cargo.toml"),
                "targets": [
                    { "src_path": repository.join("crates/tracedecay-policy/src/lib.rs") }
                ]
            },
            {
                "id": parity_id,
                "name": "tracedecay-rusqlite-parity",
                "manifest_path": repository.join("crates/tracedecay-rusqlite-parity/Cargo.toml"),
                "targets": []
            },
            {
                "id": runtime_id,
                "name": "tracedecay-rusqlite-runtime",
                "manifest_path": repository.join("crates/tracedecay-rusqlite-runtime/Cargo.toml"),
                "targets": []
            },
            {
                "id": protocol_id,
                "name": "tracedecay-sqlite-parity-protocol",
                "manifest_path": repository.join("crates/tracedecay-sqlite-parity-protocol/Cargo.toml"),
                "targets": []
            },
            {
                "id": store_id,
                "name": "tracedecay-store",
                "manifest_path": repository.join("crates/tracedecay-store/Cargo.toml"),
                "targets": [
                    { "src_path": repository.join("crates/tracedecay-store/src/lib.rs") }
                ]
            },
            {
                "id": catalog_id,
                "name": "tracedecay-tool-catalog",
                "manifest_path": repository.join("crates/tracedecay-tool-catalog/Cargo.toml"),
                "targets": [
                    { "src_path": repository.join("crates/tracedecay-tool-catalog/src/lib.rs") }
                ]
            },
            {
                "id": "registry+https://github.com/rust-lang/crates.io-index#serde@1.0.0",
                "manifest_path": "/outside/registry/serde/Cargo.toml",
                "targets": [{ "src_path": "/outside/registry/serde/src/lib.rs" }]
            }
        ],
        "workspace_members": [
            root_id,
            api_id,
            application_id,
            domain_id,
            hooks_id,
            policy_id,
            parity_id,
            runtime_id,
            protocol_id,
            store_id,
            catalog_id
        ]
    });

    let layout = parse_cargo_source_layout(
        repository,
        &serde_json::to_vec(&metadata).expect("serialize metadata fixture"),
    )
    .expect("parse metadata fixture");

    assert_eq!(
        layout.target_roots,
        [
            PathBuf::from("build.rs"),
            PathBuf::from("crates/tracedecay-api/src/lib.rs"),
            PathBuf::from("crates/tracedecay-application/src/lib.rs"),
            PathBuf::from("crates/tracedecay-domain/src/lib.rs"),
            PathBuf::from("crates/tracedecay-domain/tests/boundary.rs"),
            PathBuf::from("crates/tracedecay-hooks/src/lib.rs"),
            PathBuf::from("crates/tracedecay-policy/src/lib.rs"),
            PathBuf::from("crates/tracedecay-store/src/lib.rs"),
            PathBuf::from("crates/tracedecay-tool-catalog/src/lib.rs"),
            PathBuf::from("src/lib.rs"),
            PathBuf::from("src/main.rs"),
        ]
        .into_iter()
        .collect()
    );
    assert_eq!(
        layout.tracked_roots,
        [
            PathBuf::from("benches"),
            PathBuf::from("build.rs"),
            PathBuf::from("crates/tracedecay-api"),
            PathBuf::from("crates/tracedecay-application"),
            PathBuf::from("crates/tracedecay-domain"),
            PathBuf::from("crates/tracedecay-hooks"),
            PathBuf::from("crates/tracedecay-policy"),
            PathBuf::from("crates/tracedecay-rusqlite-parity"),
            PathBuf::from("crates/tracedecay-rusqlite-runtime"),
            PathBuf::from("crates/tracedecay-sqlite-parity-protocol"),
            PathBuf::from("crates/tracedecay-store"),
            PathBuf::from("crates/tracedecay-tool-catalog"),
            PathBuf::from("examples"),
            PathBuf::from("src"),
            PathBuf::from("tests"),
        ]
        .into_iter()
        .collect()
    );
    assert!(layout.workspace_manifests.contains(Path::new("Cargo.toml")));
    assert!(
        layout
            .workspace_manifests
            .contains(Path::new("crates/tracedecay-rusqlite-runtime/Cargo.toml"))
    );
}

#[test]
fn metadata_contract_rejects_package_aliases_extra_members_and_query_targets() {
    let temporary = tempfile::tempdir().expect("create metadata contract fixture");
    let repository = temporary.path();
    let root_id = "path+file:///workspace#root@0.1.0";
    let domain_id = "path+file:///workspace/crates/domain#domain@0.1.0";
    let store_id = "path+file:///workspace/crates/store#store@0.1.0";
    let neutral_id = "path+file:///workspace/components/engine#engine@0.1.0";
    let metadata = serde_json::json!({
        "packages": [
            {
                "id": root_id,
                "name": "tracedecay",
                "manifest_path": repository.join("Cargo.toml"),
                "dependencies": [
                    { "name": "sqlx", "rename": "serde" }
                ],
                "targets": [
                    {
                        "kind": ["lib"],
                        "name": "tracedecay",
                        "src_path": repository.join("src/lib.rs")
                    },
                    {
                        "kind": ["bin"],
                        "name": "temporal-kernel",
                        "src_path": repository.join("src/engine.rs")
                    },
                    {
                        "kind": ["example"],
                        "name": "neutral_example",
                        "src_path": repository.join("examples/neutral.rs")
                    },
                    {
                        "kind": ["test"],
                        "name": "neutral_test",
                        "src_path": repository.join("tests/neutral.rs")
                    },
                    {
                        "kind": ["custom-build"],
                        "name": "build-script-build",
                        "src_path": repository.join("build-neutral.rs")
                    }
                ]
            },
            {
                "id": domain_id,
                "name": "tracedecay-domain",
                "manifest_path": repository.join("crates/tracedecay-domain/Cargo.toml"),
                "targets": []
            },
            {
                "id": store_id,
                "name": "tracedecay-store",
                "manifest_path": repository.join("crates/tracedecay-store/Cargo.toml"),
                "dependencies": [
                    { "name": "mongodb", "rename": "serde_json" }
                ],
                "targets": []
            },
            {
                "id": neutral_id,
                "name": "engine",
                "manifest_path": repository.join("components/engine/Cargo.toml"),
                "targets": []
            }
        ],
        "workspace_members": [root_id, domain_id, store_id, neutral_id]
    });
    let layout = parse_cargo_source_layout(
        repository,
        &serde_json::to_vec(&metadata).expect("serialize metadata fixture"),
    )
    .expect("parse metadata fixture");

    for expected in [
        "alias serde",
        "package sqlx",
        "contract dependency serde_json -> mongodb",
        "temporal-kernel",
        "neutral_example",
        "neutral_test",
        "build-neutral.rs",
        "components/engine",
    ] {
        assert!(
            layout
                .boundary_violations
                .iter()
                .any(|violation| violation.contains(expected)),
            "metadata contract missed {expected}: {:?}",
            layout.boundary_violations
        );
    }
}

#[test]
fn physical_manifest_contract_classifies_paths_without_name_heuristics() {
    let temporary = tempfile::tempdir().expect("create physical manifest fixture");
    let repository = temporary.path();
    let files = [
        (
            "Cargo.toml",
            "[package]\nname = \"tracedecay\"\nversion = \"0.1.0\"\n",
        ),
        (
            "crates/tracedecay-domain/Cargo.toml",
            "[package]\nname = \"tracedecay-domain\"\nversion = \"0.1.0\"\n",
        ),
        (
            "crates/tracedecay-store/Cargo.toml",
            "[package]\nname = \"tracedecay-store\"\nversion = \"0.1.0\"\n",
        ),
        (
            "components/engine/Cargo.toml",
            "[package]\nname = \"engine\"\nversion = \"0.1.0\"\n\
             [lib]\nname = \"query_engine\"\npath = \"src/core.rs\"\n",
        ),
        (
            "vendor/upstream/Cargo.toml",
            "[package]\nname = \"query-vendor\"\nversion = \"0.1.0\"\n",
        ),
        (
            "tests/fixtures/query-project/Cargo.toml",
            "[package]\nname = \"query-fixture\"\nversion = \"0.1.0\"\n",
        ),
    ];
    for (path, source) in files {
        let path = repository.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }
    let tracked = files
        .iter()
        .map(|(path, _)| PathBuf::from(path))
        .collect::<Vec<_>>();
    let layout =
        inspect_physical_manifest_paths(repository, &tracked).expect("inspect tracked manifests");

    assert!(
        layout.violations.iter().any(|violation| {
            violation.contains("components/engine/Cargo.toml")
                && violation.contains("package engine")
                && violation.contains("lib query_engine")
        }),
        "neutral excluded package escaped the physical contract: {:?}",
        layout.violations
    );
    assert!(
        !layout
            .manifests
            .contains(Path::new("vendor/upstream/Cargo.toml"))
    );
    assert!(
        !layout
            .manifests
            .contains(Path::new("tests/fixtures/query-project/Cargo.toml"))
    );
}

#[cfg(unix)]
#[test]
fn physical_manifest_contract_rejects_symlinked_crates() {
    let temporary = tempfile::tempdir().expect("create symlinked manifest fixture");
    let repository = temporary.path();
    for path in TEST_WORKSPACE_MANIFESTS {
        let path = repository.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n").unwrap();
    }
    fs::create_dir_all(repository.join("components")).unwrap();
    symlink(
        repository.join("crates/tracedecay-domain"),
        repository.join("components/engine"),
    )
    .unwrap();
    let mut tracked = TEST_WORKSPACE_MANIFESTS
        .iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    tracked.push(PathBuf::from("components/engine"));

    let layout =
        inspect_physical_manifest_paths(repository, &tracked).expect("inspect symlinked manifest");
    assert!(
        layout
            .violations
            .iter()
            .any(|violation| violation.contains("symlink aliases the same physical crate")),
        "symlinked crate escaped canonical-path inspection: {:?}",
        layout.violations
    );
}

#[cfg(unix)]
#[test]
fn physical_manifest_contract_rejects_outside_rust_symlinks() {
    let temporary = tempfile::tempdir().expect("create Rust symlink fixture");
    let repository = temporary.path().join("repository");
    fs::create_dir_all(repository.join("src/query")).unwrap();
    let outside = temporary.path().join("outside.rs");
    fs::write(&outside, "sqlx::Pool::connect();\n").unwrap();
    symlink(&outside, repository.join("src/query/linked.rs")).unwrap();

    let layout =
        inspect_physical_manifest_paths(&repository, &[PathBuf::from("src/query/linked.rs")])
            .expect("inspect tracked Rust symlink");
    assert!(
        layout
            .violations
            .iter()
            .any(|violation| violation.contains("outside the repository")),
        "outside Rust symlink escaped canonical inspection: {:?}",
        layout.violations
    );
}

#[cfg(unix)]
#[test]
fn physical_manifest_contract_discovers_inside_rust_symlinks() {
    let temporary = tempfile::tempdir().expect("create inside Rust symlink fixture");
    let repository = temporary.path();
    for path in TEST_WORKSPACE_MANIFESTS {
        let path = repository.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n").unwrap();
    }
    fs::create_dir_all(repository.join("src/query")).unwrap();
    fs::create_dir_all(repository.join("shared")).unwrap();
    fs::write(repository.join("src/query/mod.rs"), "mod safe;\n").unwrap();
    fs::write(repository.join("shared/safe.rs"), "pub struct Safe;\n").unwrap();
    symlink(
        repository.join("shared/safe.rs"),
        repository.join("src/query/safe.rs"),
    )
    .unwrap();
    let mut tracked = TEST_WORKSPACE_MANIFESTS
        .iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    tracked.push(PathBuf::from("src/query/safe.rs"));

    let layout =
        inspect_physical_manifest_paths(repository, &tracked).expect("inspect inside Rust symlink");
    assert!(
        layout.violations.is_empty(),
        "inside Rust symlink should be inspectable: {:?}",
        layout.violations
    );
    assert!(
        layout
            .symlinked_rust_sources
            .contains(Path::new("src/query/safe.rs"))
    );
    let sources = [
        PathBuf::from("src/query/mod.rs"),
        PathBuf::from("src/query/safe.rs"),
    ]
    .into_iter()
    .collect();
    assert!(
        query_kernel_violations(repository, &sources)
            .expect("inspect in-repository symlinked Rust source")
            .is_empty(),
        "in-repository Rust symlink target must be fully scanned"
    );
}
