//! Resolved compile-boundary enforcement for `tracedecay-query`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::env;
use std::path::PathBuf;
use std::process::Command;

use serde::Deserialize;

#[derive(Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
    workspace_members: BTreeSet<String>,
    resolve: CargoResolve,
}

#[derive(Deserialize)]
struct CargoPackage {
    id: String,
    name: String,
}

#[derive(Deserialize)]
struct CargoResolve {
    nodes: Vec<ResolveNode>,
}

#[derive(Deserialize)]
struct ResolveNode {
    id: String,
    deps: Vec<ResolveDependency>,
}

#[derive(Deserialize)]
struct ResolveDependency {
    pkg: String,
    dep_kinds: Vec<ResolveDependencyKind>,
}

#[derive(Deserialize)]
struct ResolveDependencyKind {
    kind: Option<String>,
}

fn cargo_metadata_without_default_features() -> CargoMetadata {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")))
        .args(["metadata", "--format-version=1", "--no-default-features"])
        .output()
        .expect("run cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata --no-default-features failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse cargo metadata")
}

fn participates_in_compile_graph(dependency: &ResolveDependency) -> bool {
    dependency
        .dep_kinds
        .iter()
        .any(|kind| kind.kind.as_deref() != Some("dev"))
}

#[test]
fn query_resolved_graph_excludes_policy_without_default_features() {
    let metadata = cargo_metadata_without_default_features();
    let query_packages = metadata
        .packages
        .iter()
        .filter(|package| {
            package.name == "tracedecay-query"
                && metadata.workspace_members.contains(package.id.as_str())
        })
        .collect::<Vec<_>>();
    assert_eq!(
        query_packages.len(),
        1,
        "exactly one tracedecay-query workspace package must resolve"
    );
    assert!(
        metadata
            .packages
            .iter()
            .any(|package| package.name == "tracedecay-policy"),
        "tracedecay-policy must be present in metadata so this boundary check is meaningful"
    );
    let query = query_packages[0];

    let package_names = metadata
        .packages
        .iter()
        .map(|package| (package.id.as_str(), package.name.as_str()))
        .collect::<BTreeMap<_, _>>();
    let nodes = metadata
        .resolve
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect::<BTreeMap<_, _>>();
    let query_node = nodes
        .get(query.id.as_str())
        .expect("tracedecay-query must have a resolved dependency node");
    assert!(
        query_node.deps.iter().any(participates_in_compile_graph),
        "tracedecay-query must resolve a nonempty compile dependency graph"
    );

    let mut queue = VecDeque::from([query.id.as_str()]);
    let mut seen = BTreeSet::from([query.id.as_str()]);
    while let Some(package_id) = queue.pop_front() {
        let package_name = package_names
            .get(package_id)
            .copied()
            .expect("every resolved node must identify a package");
        assert_ne!(
            package_name, "tracedecay-policy",
            "tracedecay-query must not resolve tracedecay-policy through a direct or transitive compile dependency"
        );

        let node = nodes
            .get(package_id)
            .expect("every reached package must have a resolved node");
        for dependency in node
            .deps
            .iter()
            .filter(|dependency| participates_in_compile_graph(dependency))
        {
            let dependency_id = dependency.pkg.as_str();
            assert!(
                package_names.contains_key(dependency_id),
                "resolved dependency must identify a package: {dependency_id}"
            );
            if seen.insert(dependency_id) {
                queue.push_back(dependency_id);
            }
        }
    }

    assert!(
        seen.len() > 1,
        "tracedecay-query compile graph must include resolved dependencies"
    );
}
