use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Architecture {
    version: u32,
    package_ceiling: usize,
    generated_views: Vec<String>,
    owners: BTreeMap<String, Owner>,
    edges: Vec<Edge>,
    release: Release,
    governance: Governance,
    budgets: Budgets,
    public_facades: Vec<PublicFacade>,
    capabilities: Vec<Capability>,
    stores: Vec<Store>,
    replaced_v1_clusters: Vec<Replacement>,
    adapter_contracts: Vec<AdapterContract>,
    deletion_waves: Vec<DeletionWave>,
    scorecard: Vec<ScorecardMetric>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Owner {
    kind: String,
    path: String,
    plan: String,
    release_tier: u8,
    #[serde(default)]
    allowed_dependencies: Vec<String>,
    #[serde(default)]
    forbidden_dependencies: Vec<String>,
    #[serde(default)]
    forbidden_source_patterns: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Edge {
    from: String,
    to: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Release {
    compatibility_gate: String,
    rollback_gate: String,
    v1_removal_gate: String,
    waves: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Governance {
    inventory_generator: String,
    inventory_output: String,
    adapter_final_pr: String,
    adapter_final_state: String,
    package_admission_rule: String,
    waiver_rule: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Budgets {
    rust_package_ceiling: usize,
    definite_duplicate_body_lines: usize,
    default_binary_ratio_max: f64,
    idle_rss_ratio_max: f64,
    hot_build_ratio_max: f64,
    clean_build_ratio_max: f64,
    parity_replacement: String,
    complexity: String,
    generated_accounting: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicFacade {
    id: String,
    owner: String,
    consumers: Vec<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Capability {
    id: String,
    owner: String,
    facade: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Store {
    id: String,
    owner: String,
    writer: String,
    readers: Vec<String>,
    classification: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Replacement {
    id: String,
    owner: String,
    disposition: String,
    delete_by_pr: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdapterContract {
    id: String,
    owner: String,
    required_fields: Vec<String>,
    delete_by_pr: String,
    new_callers_forbidden: bool,
    policy_forbidden: bool,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeletionWave {
    id: String,
    replaced_cluster: String,
    delete_by_pr: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScorecardMetric {
    metric: String,
    detector: String,
    target: String,
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
    let declared: BTreeSet<_> = architecture
        .edges
        .iter()
        .map(|edge| (edge.from.as_str(), edge.to.as_str()))
        .collect();
    let allowed: BTreeSet<_> = architecture
        .owners
        .iter()
        .flat_map(|(owner, policy)| {
            policy
                .allowed_dependencies
                .iter()
                .map(move |dependency| (owner.as_str(), dependency.as_str()))
        })
        .collect();
    assert_eq!(
        declared.len(),
        architecture.edges.len(),
        "duplicate DAG edge"
    );
    assert_eq!(
        declared, allowed,
        "DAG must be the complete allowed edge set"
    );
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
    let forbidden: BTreeSet<&str> = ["store", "projectors", "query", "policy", "code-index"]
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
    assert_eq!(architecture.generated_views.len(), 5);
    for view in &architecture.generated_views {
        let text =
            fs::read_to_string(view).unwrap_or_else(|_| panic!("missing generated view {view}"));
        assert!(text.contains("Generated from architecture-boundaries.toml; do not edit."));
    }
    let status = Command::new("python3")
        .args(["scripts/generate_architecture_views.py", "--check"])
        .status()
        .expect("python3 is required to check deterministic architecture views");
    assert!(status.success(), "generated architecture views drifted");
    assert!(!architecture.release.compatibility_gate.is_empty());
    assert!(!architecture.release.rollback_gate.is_empty());
    assert!(!architecture.release.v1_removal_gate.is_empty());
    assert!(architecture.release.waves.len() >= 4);
}

#[test]
fn machine_authority_has_complete_governance_schema() {
    let architecture = load();
    assert_eq!(architecture.governance.inventory_generator, "plan-12-pr-3r");
    assert!(
        architecture
            .governance
            .inventory_output
            .starts_with("target/")
    );
    assert_eq!(architecture.governance.adapter_final_pr, "PR 37");
    assert!(
        architecture
            .governance
            .adapter_final_state
            .contains("zero-live")
    );
    assert!(!architecture.governance.package_admission_rule.is_empty());
    assert!(architecture.governance.waiver_rule.contains("PR-37"));
    assert_eq!(
        architecture.budgets.rust_package_ceiling,
        architecture.package_ceiling
    );
    assert_eq!(architecture.budgets.definite_duplicate_body_lines, 10);
    assert_eq!(architecture.budgets.default_binary_ratio_max, 1.25);
    assert_eq!(architecture.budgets.idle_rss_ratio_max, 1.25);
    assert_eq!(architecture.budgets.hot_build_ratio_max, 1.25);
    assert_eq!(architecture.budgets.clean_build_ratio_max, 1.5);
    assert!(!architecture.budgets.parity_replacement.is_empty());
    assert!(!architecture.budgets.complexity.is_empty());
    assert!(!architecture.budgets.generated_accounting.is_empty());
    let facades: BTreeSet<_> = architecture
        .public_facades
        .iter()
        .map(|item| item.id.as_str())
        .collect();
    for facade in &architecture.public_facades {
        assert!(architecture.owners.contains_key(&facade.owner));
        assert!(!facade.consumers.is_empty());
        assert!(
            facade
                .consumers
                .iter()
                .all(|owner| architecture.owners.contains_key(owner))
        );
    }
    for capability in &architecture.capabilities {
        assert!(!capability.id.is_empty());
        assert!(architecture.owners.contains_key(&capability.owner));
        assert!(facades.contains(capability.facade.as_str()));
    }
    for store in &architecture.stores {
        assert!(!store.id.is_empty());
        assert!(architecture.owners.contains_key(&store.owner));
        assert!(architecture.owners.contains_key(&store.writer));
        assert!(
            store
                .readers
                .iter()
                .all(|owner| architecture.owners.contains_key(owner))
        );
        assert!(["canonical", "rebuildable-derived"].contains(&store.classification.as_str()));
    }
    let replacements: BTreeSet<_> = architecture
        .replaced_v1_clusters
        .iter()
        .map(|item| item.id.as_str())
        .collect();
    assert_eq!(replacements.len(), 7);
    for replacement in &architecture.replaced_v1_clusters {
        assert!(architecture.owners.contains_key(&replacement.owner));
        assert!(["replace", "delete"].contains(&replacement.disposition.as_str()));
        assert!(replacement.delete_by_pr.starts_with("PR "));
    }
    for wave in &architecture.deletion_waves {
        assert!(!wave.id.is_empty());
        assert!(replacements.contains(wave.replaced_cluster.as_str()));
        assert!(wave.delete_by_pr.starts_with("PR "));
    }
    let adapter = &architecture.adapter_contracts[0];
    assert_eq!(adapter.id, "required-ledger-schema");
    assert!(architecture.owners.contains_key(&adapter.owner));
    assert_eq!(adapter.required_fields.len(), 14);
    assert_eq!(adapter.delete_by_pr, "PR 37");
    assert!(adapter.new_callers_forbidden && adapter.policy_forbidden);
    assert_eq!(architecture.scorecard.len(), 28);
    for metric in &architecture.scorecard {
        assert!(
            !metric.metric.is_empty() && !metric.detector.is_empty() && !metric.target.is_empty()
        );
    }
}

#[test]
fn cargo_and_source_policy_enforce_materialized_boundaries() {
    let architecture = load();
    let output = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .output()
        .expect("cargo metadata must run");
    assert!(output.status.success());
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let packages = metadata["packages"].as_array().unwrap();
    assert!(packages.len() <= architecture.package_ceiling);
    for (name, owner) in &architecture.owners {
        let path = Path::new(&owner.path);
        if owner.kind == "rust-package" && path.join("Cargo.toml").exists() {
            let canonical = path.canonicalize().unwrap();
            assert!(
                packages.iter().any(|package| Path::new(
                    package["manifest_path"].as_str().unwrap()
                )
                .parent()
                    == Some(canonical.as_path())),
                "materialized package {name} missing from cargo metadata"
            );
        }
        if owner.kind == "root-private-module" && path.exists() {
            let mut pending = vec![path.to_path_buf()];
            while let Some(entry) = pending.pop() {
                if entry.is_dir() {
                    pending.extend(
                        fs::read_dir(entry)
                            .unwrap()
                            .map(|item| item.unwrap().path()),
                    );
                } else if entry.extension().is_some_and(|ext| ext == "rs") {
                    let source = fs::read_to_string(&entry).unwrap();
                    for pattern in &owner.forbidden_source_patterns {
                        assert!(
                            !source.contains(pattern),
                            "{name} source {} imports forbidden {pattern}",
                            entry.display()
                        );
                    }
                }
            }
        }
    }
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
        if name == "frontend-build-and-embedding.md" {
            assert!(text.contains("Pending measured Rsbuild-versus-Vite comparison"));
            assert!(text.contains("evidence/frontend-build-comparison.md"));
            assert!(!text.contains("Accepted for V2 Phase 0 after a measured"));
        }
    }
}
