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

    for package in ["tracedecay-domain", "tracedecay-store"] {
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
