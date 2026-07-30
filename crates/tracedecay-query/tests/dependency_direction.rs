//! Compile-boundary enforcement: `tracedecay-query` must not depend on
//! `tracedecay-policy` through declared or resolved package metadata.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
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
    kind: Option<String>,
}

#[derive(Deserialize)]
struct CargoResolve {
    nodes: Vec<ResolveNode>,
}

#[derive(Deserialize)]
struct ResolveNode {
    id: String,
    dependencies: Vec<String>,
}

fn cargo_metadata() -> CargoMetadata {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")))
        .args(["metadata", "--format-version=1", "--no-default-features"])
        .output()
        .expect("run cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse cargo metadata")
}

#[test]
fn query_leaf_has_no_policy_dependency() {
    let metadata = cargo_metadata();
    let query = metadata
        .packages
        .iter()
        .find(|package| package.name == "tracedecay-query")
        .expect("tracedecay-query package");

    let declared = query
        .dependencies
        .iter()
        .filter(|dependency| {
            dependency
                .kind
                .as_deref()
                .is_none_or(|kind| kind != "dev" && kind != "build")
        })
        .map(|dependency| dependency.name.as_str())
        .collect::<BTreeSet<_>>();
    assert!(
        !declared.contains("tracedecay-policy"),
        "tracedecay-query must not declare a tracedecay-policy dependency; accept policy through its public API"
    );

    let package_names = metadata
        .packages
        .iter()
        .map(|package| (package.id.as_str(), package.name.as_str()))
        .collect::<BTreeMap<_, _>>();
    let edges = metadata
        .resolve
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node.dependencies.as_slice()))
        .collect::<BTreeMap<_, _>>();

    let mut queue = VecDeque::from([query.id.as_str()]);
    let mut seen = BTreeSet::from([query.id.as_str()]);
    while let Some(package_id) = queue.pop_front() {
        let name = package_names
            .get(package_id)
            .copied()
            .unwrap_or(package_id);
        assert_ne!(
            name, "tracedecay-policy",
            "tracedecay-query resolved dependency graph must not include tracedecay-policy"
        );
        if let Some(dependencies) = edges.get(package_id) {
            for dependency_id in *dependencies {
                if seen.insert(dependency_id) {
                    queue.push_back(dependency_id);
                }
            }
        }
    }
}
