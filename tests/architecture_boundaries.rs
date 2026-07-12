use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

#[derive(Debug, Deserialize)]
struct Architecture {
    version: u32,
    package_ceiling: usize,
    generated_views: Vec<String>,
    owners: BTreeMap<String, Owner>,
    edges: Vec<Edge>,
    release: Release,
}

#[derive(Debug, Deserialize)]
struct Owner {
    kind: String,
    path: String,
    plan: String,
    release_tier: u8,
    #[serde(default)]
    allowed_dependencies: Vec<String>,
    #[serde(default)]
    forbidden_dependencies: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Edge {
    from: String,
    to: String,
}

#[derive(Debug, Deserialize)]
struct Release {
    compatibility_gate: String,
    rollback_gate: String,
    v1_removal_gate: String,
    waves: Vec<String>,
}

fn load() -> Architecture {
    let text = fs::read_to_string("architecture-boundaries.toml")
        .expect("architecture-boundaries.toml is required");
    toml::from_str(&text).expect("architecture-boundaries.toml must parse")
}

#[test]
fn architecture_manifest_has_bounded_acyclic_owners() {
    let architecture = load();
    assert_eq!(architecture.version, 1);
    let packages = architecture
        .owners
        .values()
        .filter(|owner| owner.kind == "rust-package")
        .count();
    assert!(packages <= architecture.package_ceiling);
    assert!(architecture.package_ceiling <= 11);

    for (name, owner) in &architecture.owners {
        assert!(
            Path::new(&owner.plan).exists(),
            "{name} has missing plan {}",
            owner.plan
        );
        assert!(!owner.path.is_empty(), "{name} has no target path");
        assert!(owner.release_tier > 0, "{name} has no release tier");
        for dependency in &owner.allowed_dependencies {
            assert!(
                architecture.owners.contains_key(dependency),
                "{name} allows unknown dependency {dependency}"
            );
        }
    }

    let mut incoming: BTreeMap<&str, usize> = architecture
        .owners
        .keys()
        .map(|name| (name.as_str(), 0))
        .collect();
    let mut outgoing: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for edge in &architecture.edges {
        assert!(architecture.owners.contains_key(&edge.from));
        assert!(architecture.owners.contains_key(&edge.to));
        assert_ne!(edge.from, edge.to);
        assert!(
            architecture.owners[&edge.from]
                .allowed_dependencies
                .contains(&edge.to)
        );
        *incoming.get_mut(edge.from.as_str()).unwrap() += 1;
        outgoing
            .entry(edge.to.as_str())
            .or_default()
            .push(edge.from.as_str());
    }
    let mut ready: Vec<&str> = incoming
        .iter()
        .filter_map(|(node, degree)| (*degree == 0).then_some(*node))
        .collect();
    let mut visited = 0;
    while let Some(node) = ready.pop() {
        visited += 1;
        for dependent in outgoing.get(node).into_iter().flatten() {
            let degree = incoming.get_mut(dependent).unwrap();
            *degree -= 1;
            if *degree == 0 {
                ready.push(dependent);
            }
        }
    }
    assert_eq!(
        visited,
        architecture.owners.len(),
        "architecture dependency graph contains a cycle"
    );
}

#[test]
fn transports_are_isolated_from_storage_and_business_implementations() {
    let architecture = load();
    let forbidden: BTreeSet<&str> = [
        "store",
        "capture",
        "projectors",
        "query",
        "policy",
        "code-index",
    ]
    .into_iter()
    .collect();
    for transport in [
        "api",
        "hooks",
        "presentation",
        "host-deploy",
        "remote-brain-transport",
        "client-rust",
        "client-typescript",
        "client-python",
        "dashboard",
    ] {
        let owner = &architecture.owners[transport];
        for dependency in &owner.allowed_dependencies {
            assert!(
                !forbidden.contains(dependency.as_str()),
                "transport {transport} bypasses application through {dependency}"
            );
        }
        assert!(
            owner
                .forbidden_dependencies
                .iter()
                .any(|item| item == "store-implementation")
        );
    }
}

#[test]
fn generated_views_and_release_gates_are_checked_in() {
    let architecture = load();
    assert_eq!(architecture.generated_views.len(), 3);
    for view in &architecture.generated_views {
        let text =
            fs::read_to_string(view).unwrap_or_else(|_| panic!("missing generated view {view}"));
        assert!(
            text.starts_with("<!-- Generated from architecture-boundaries.toml; do not edit. -->")
        );
    }
    assert!(!architecture.release.compatibility_gate.is_empty());
    assert!(!architecture.release.rollback_gate.is_empty());
    assert!(!architecture.release.v1_removal_gate.is_empty());
    assert!(architecture.release.waves.len() >= 4);
}

#[test]
fn seven_v2_adrs_lock_the_phase_zero_decisions() {
    let required = [
        "logical-brain.md",
        "identity-and-evidence.md",
        "storage-and-consistency.md",
        "query-and-api.md",
        "privacy-and-retention.md",
        "dashboard-and-renderers.md",
        "frontend-build-and-embedding.md",
    ];
    for name in required {
        let path = format!("docs/architecture/v2/{name}");
        let text = fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing ADR {path}"));
        for heading in [
            "## Status",
            "## Decision",
            "## Rejected alternatives",
            "## Compatibility, rollback, and removal gates",
        ] {
            assert!(text.contains(heading), "{path} omits {heading}");
        }
    }
}
