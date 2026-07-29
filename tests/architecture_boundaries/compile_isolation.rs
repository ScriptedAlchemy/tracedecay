//! Compile-graph isolation for packages that do not own code indexing.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::path::PathBuf;
use std::process::Command;

use serde::Deserialize;

#[derive(Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
    resolve: CargoResolve,
}

#[derive(Deserialize)]
struct CargoPackage {
    id: String,
    name: String,
    dependencies: Vec<CargoDependency>,
}

#[derive(Deserialize)]
struct CargoDependency {
    name: String,
}

#[derive(Deserialize)]
struct CargoResolve {
    nodes: Vec<CargoResolveNode>,
}

#[derive(Deserialize)]
struct CargoResolveNode {
    id: String,
    dependencies: Vec<String>,
}

fn cargo_metadata() -> CargoMetadata {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new(cargo)
        .current_dir(repository)
        .args(["metadata", "--format-version=1", "--no-default-features"])
        .output()
        .expect("run stock cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse cargo metadata")
}

fn dependency_closure(metadata: &CargoMetadata, package_name: &str) -> BTreeSet<String> {
    let package_names = metadata
        .packages
        .iter()
        .map(|package| (package.id.as_str(), package.name.as_str()))
        .collect::<BTreeMap<_, _>>();
    let dependencies = metadata
        .resolve
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node.dependencies.as_slice()))
        .collect::<BTreeMap<_, _>>();
    let root = metadata
        .packages
        .iter()
        .find(|package| package.name == package_name)
        .unwrap_or_else(|| panic!("workspace package {package_name}"))
        .id
        .as_str();

    let mut pending = vec![root];
    let mut seen = BTreeSet::new();
    let mut names = BTreeSet::new();
    while let Some(package_id) = pending.pop() {
        if !seen.insert(package_id) {
            continue;
        }
        names.insert(
            package_names
                .get(package_id)
                .unwrap_or_else(|| panic!("resolved package {package_id}"))
                .to_string(),
        );
        if let Some(children) = dependencies.get(package_id) {
            pending.extend(children.iter().map(String::as_str));
        }
    }
    names
}

fn direct_dependencies(metadata: &CargoMetadata, package_name: &str) -> BTreeSet<String> {
    metadata
        .packages
        .iter()
        .find(|package| package.name == package_name)
        .unwrap_or_else(|| panic!("workspace package {package_name}"))
        .dependencies
        .iter()
        .map(|dependency| dependency.name.clone())
        .collect()
}

#[test]
fn non_indexing_packages_exclude_grammars_structural_search_and_root_indexer() {
    let metadata = cargo_metadata();
    let forbidden_exact = BTreeSet::from([
        "ast-grep-core",
        "tokensave-large-treesitters",
        "tokensave-medium-treesitters",
        "tracedecay",
        "tree-sitter",
        "tree-sitter-language",
    ]);

    for package in [
        "tracedecay-application",
        "tracedecay-domain",
        "tracedecay-policy",
        "tracedecay-store",
    ] {
        let closure = dependency_closure(&metadata, package);
        let violations = closure
            .iter()
            .filter(|dependency| {
                forbidden_exact.contains(dependency.as_str())
                    || dependency.starts_with("tree-sitter-")
            })
            .cloned()
            .collect::<Vec<_>>();
        assert!(
            violations.is_empty(),
            "{package} must not compile grammar, structural-search, or root code-index packages: {}",
            violations.join(", ")
        );
    }
}

#[test]
fn code_index_dependencies_are_explicit_and_root_free() {
    let metadata = cargo_metadata();
    let direct = direct_dependencies(&metadata, "tracedecay-code-index");
    let expected = [
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
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    assert_eq!(direct, expected);
    assert!(!direct.contains("tracedecay"));
}

#[test]
fn root_uses_code_index_facades_instead_of_inline_sources() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for path in ["src/code_index", "src/extraction", "src/ast_grep_search.rs"] {
        assert!(
            !root.join(path).exists(),
            "root must not retain extracted code-index source at {path}"
        );
    }
    let lib = std::fs::read_to_string(root.join("src/lib.rs")).expect("read root lib facade");
    assert!(lib.contains("pub use tracedecay_code_index as code_index;"));
    assert!(lib.contains("pub use tracedecay_code_index::extraction;"));
    assert!(lib.contains("pub use tracedecay_code_index::ast_grep_search;"));
}

#[test]
fn query_dependencies_are_explicit_and_root_free() {
    let metadata = cargo_metadata();
    let direct = direct_dependencies(&metadata, "tracedecay-query");
    let expected = [
        "hex",
        "hmac",
        "serde",
        "serde_json",
        "sha2",
        "static_assertions",
        "thiserror",
        "tracedecay-code-index",
        "tracedecay-domain",
        "tracedecay-policy",
        "zeroize",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    assert_eq!(direct, expected);
    assert!(!direct.contains("tracedecay"));
}

#[test]
fn root_uses_query_facade_instead_of_inline_sources() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    assert!(
        !root.join("src/query").exists(),
        "root must not retain extracted query source"
    );
    let lib = std::fs::read_to_string(root.join("src/lib.rs")).expect("read root lib facade");
    assert!(lib.contains("pub use tracedecay_query as query;"));
}

#[test]
fn domain_dependencies_are_exactly_the_pure_value_allowlist() {
    let metadata = cargo_metadata();
    let direct = direct_dependencies(&metadata, "tracedecay-domain");
    let expected_direct = [
        "schemars",
        "serde",
        "serde_json",
        "sha2",
        "thiserror",
        "url",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    assert_eq!(
        direct, expected_direct,
        "tracedecay-domain must not depend on I/O, stores, transports, providers, settings, credentials, lifecycle, UI, or the root crate"
    );
}

#[test]
fn policy_package_has_only_pure_value_dependencies() {
    let metadata = cargo_metadata();
    let policy = metadata
        .packages
        .iter()
        .find(|package| package.name == "tracedecay-policy")
        .expect("workspace policy package");
    let actual = policy
        .dependencies
        .iter()
        .map(|dependency| dependency.name.as_str())
        .collect::<BTreeSet<_>>();
    let expected = BTreeSet::from(["serde", "serde_json", "tracedecay-domain"]);

    assert_eq!(
        actual, expected,
        "policy must not depend on I/O, stores, transports, models, or configuration resolution"
    );
}

#[test]
fn store_dependencies_are_exactly_the_contract_allowlist() {
    let metadata = cargo_metadata();
    let direct = direct_dependencies(&metadata, "tracedecay-store");
    let expected_direct = ["serde", "serde_json", "thiserror", "tracedecay-domain"]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        direct, expected_direct,
        "tracedecay-store dependencies must remain exactly contract-only"
    );
}

#[test]
fn application_dependencies_are_exactly_the_use_case_allowlist() {
    let metadata = cargo_metadata();
    let direct = direct_dependencies(&metadata, "tracedecay-application");
    let expected_direct = [
        "schemars",
        "serde",
        "serde_json",
        "thiserror",
        "tracedecay-domain",
        "tracedecay-policy",
        "tracedecay-tool-catalog",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    assert_eq!(
        direct, expected_direct,
        "tracedecay-application must depend only on domain, policy, catalog, and value libraries"
    );
}

#[test]
fn api_dependencies_are_exactly_the_thin_adapter_allowlist() {
    let metadata = cargo_metadata();
    let direct = direct_dependencies(&metadata, "tracedecay-api");
    let expected_direct = [
        "axum",
        "futures-util",
        "serde",
        "serde_json",
        "thiserror",
        "tracedecay-application",
        "tracedecay-tool-catalog",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    assert_eq!(
        direct, expected_direct,
        "tracedecay-api must remain a transport adapter over application and catalog contracts"
    );
}
