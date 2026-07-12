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

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
    workspace_members: BTreeSet<String>,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    id: String,
    manifest_path: String,
    dependencies: Vec<CargoDependency>,
}

#[derive(Debug, Deserialize)]
struct CargoDependency {
    name: String,
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DependencyPolicy {
    version: u32,
    owners: BTreeMap<String, DependencyOwner>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DependencyOwner {
    path: String,
    allowed: Vec<String>,
    forbidden: Vec<String>,
    forbidden_source_patterns: Vec<String>,
}

fn load() -> Architecture {
    let text = fs::read_to_string("architecture-boundaries.toml")
        .expect("architecture-boundaries.toml is required");
    toml::from_str(&text).expect("architecture-boundaries.toml must parse")
}

fn canonical(path: &Path) -> Result<std::path::PathBuf, String> {
    path.canonicalize()
        .map_err(|error| format!("cannot canonicalize {}: {error}", path.display()))
}

fn validate_cargo_edges(
    architecture: &Architecture,
    metadata: &CargoMetadata,
) -> Result<(), String> {
    let mut materialized = BTreeMap::new();
    for (name, owner) in &architecture.owners {
        if owner.kind == "rust-package" && Path::new(&owner.path).join("Cargo.toml").exists() {
            materialized.insert(canonical(Path::new(&owner.path))?, name.as_str());
        }
    }

    for (package_path, name) in materialized {
        let package = metadata
            .packages
            .iter()
            .find(|package| {
                Path::new(&package.manifest_path)
                    .parent()
                    .and_then(|path| path.canonicalize().ok())
                    .as_ref()
                    == Some(&package_path)
            })
            .ok_or_else(|| format!("materialized package {name} missing from cargo metadata"))?;
        if !metadata.workspace_members.contains(&package.id) {
            return Err(format!(
                "materialized package {name} is not a workspace member"
            ));
        }
        let owner = &architecture.owners[name];
        for dependency in package
            .dependencies
            .iter()
            .filter(|dependency| dependency.path.is_some())
        {
            let dependency_path = canonical(Path::new(dependency.path.as_deref().unwrap()))?;
            let dependency_owner = architecture
                .owners
                .iter()
                .find_map(|(candidate, policy)| {
                    (policy.kind == "rust-package"
                        && Path::new(&policy.path).canonicalize().ok().as_ref()
                            == Some(&dependency_path))
                    .then_some(candidate.as_str())
                })
                .ok_or_else(|| {
                    format!(
                        "{name} has unowned local Cargo dependency {}",
                        dependency.name
                    )
                })?;
            if !owner
                .allowed_dependencies
                .iter()
                .any(|allowed| allowed == dependency_owner)
            {
                return Err(format!(
                    "{name} has forbidden real Cargo edge to {dependency_owner}"
                ));
            }
        }
    }
    Ok(())
}

fn validate_source(name: &str, owner: &Owner, path: &Path, source: &str) -> Result<(), String> {
    for pattern in &owner.forbidden_source_patterns {
        if source.contains(pattern) {
            return Err(format!(
                "{name} source {} imports forbidden {pattern}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn validate_owner_imports(
    architecture: &Architecture,
    name: &str,
    owner: &Owner,
    path: &Path,
    source: &str,
) -> Result<(), String> {
    validate_source(name, owner, path, source)?;
    for (dependency_name, dependency) in &architecture.owners {
        if dependency_name == name || dependency.kind != "rust-package" {
            continue;
        }
        let crate_name = Path::new(&dependency.path)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or(dependency_name)
            .replace('-', "_");
        let imported = [
            format!("use {crate_name}::"),
            format!("{crate_name}::"),
            format!("extern crate {crate_name}"),
        ]
        .iter()
        .any(|pattern| source.contains(pattern));
        if imported
            && !owner
                .allowed_dependencies
                .iter()
                .any(|allowed| allowed == dependency_name)
        {
            return Err(format!(
                "{name} source {} has forbidden real import of {dependency_name}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn rust_sources(path: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    let mut pending = Vec::new();
    let mut sources = Vec::new();
    if path.exists() {
        pending.push(path.to_path_buf());
    }
    let sibling = path.with_extension("rs");
    if sibling.exists() {
        pending.push(sibling);
    }
    while let Some(entry) = pending.pop() {
        if entry.is_dir() {
            pending.extend(
                fs::read_dir(&entry)
                    .map_err(|error| format!("cannot read {}: {error}", entry.display()))?
                    .map(|item| item.map(|item| item.path()))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|error| format!("cannot walk {}: {error}", entry.display()))?,
            );
        } else if entry.extension().is_some_and(|extension| extension == "rs") {
            sources.push(entry);
        }
    }
    sources.sort();
    sources.dedup();
    Ok(sources)
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
    let policy_text = fs::read_to_string("architecture-dependency-policy.toml")
        .expect("generated dependency policy is required");
    let policy: DependencyPolicy =
        toml::from_str(&policy_text).expect("generated dependency policy must parse");
    assert_eq!(policy.version, architecture.version);
    assert_eq!(policy.owners.len(), architecture.owners.len());
    for (name, generated) in &policy.owners {
        let authority = &architecture.owners[name];
        assert_eq!(generated.path, authority.path);
        assert_eq!(generated.allowed, authority.allowed_dependencies);
        assert_eq!(generated.forbidden, authority.forbidden_dependencies);
        assert_eq!(
            generated.forbidden_source_patterns,
            authority.forbidden_source_patterns
        );
    }
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
    let metadata: CargoMetadata = serde_json::from_slice(&output.stdout).unwrap();
    assert!(metadata.packages.len() <= architecture.package_ceiling);
    validate_cargo_edges(&architecture, &metadata).unwrap();
    for (name, owner) in &architecture.owners {
        let path = Path::new(&owner.path);
        if owner.kind == "root-private-module" {
            for entry in rust_sources(path).unwrap() {
                let source = fs::read_to_string(&entry).unwrap();
                validate_owner_imports(&architecture, name, owner, &entry, &source).unwrap();
            }
        }
    }
}

#[test]
fn forbidden_source_pattern_is_rejected_by_focused_fixture() {
    let architecture = load();
    let owner = &architecture.owners["api"];
    let error = validate_source(
        "api",
        owner,
        Path::new("src/v2/api/fixture.rs"),
        "use libsql::Connection;",
    )
    .unwrap_err();
    assert!(error.contains("imports forbidden libsql::"));
}

#[test]
fn dependency_policy_generator_escapes_apostrophes_as_valid_toml() {
    let script = r#"
import importlib.util
import pathlib
import tomllib
path = pathlib.Path('scripts/generate_architecture_views.py')
spec = importlib.util.spec_from_file_location('architecture_generator', path)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
rendered = module.dependency_policy({'version': 1, 'owners': {'owner': {
    'path': "crates/owner's-package",
    'allowed_dependencies': ["friend's-owner"],
    'forbidden_dependencies': ["stranger's-owner"],
    'forbidden_source_patterns': ["Owner'sType"],
}}})
parsed = tomllib.loads(rendered)
assert parsed['owners']['owner']['path'] == "crates/owner's-package"
"#;
    let status = Command::new("python3")
        .args(["-c", script])
        .status()
        .expect("python3 is required to verify generated TOML escaping");
    assert!(
        status.success(),
        "apostrophe fixture generated invalid TOML"
    );
}

#[test]
fn forbidden_owner_import_is_rejected_by_focused_fixture() {
    let architecture = load();
    let owner = &architecture.owners["api"];
    let error = validate_owner_imports(
        &architecture,
        "api",
        owner,
        Path::new("src/v2/api/fixture.rs"),
        "use tracedecay_store::Store;",
    )
    .unwrap_err();
    assert!(error.contains("forbidden real import of store"));
}

#[test]
fn forbidden_real_cargo_edge_is_rejected_by_focused_fixture() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("root");
    let domain = temporary.path().join("domain");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&domain).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname='root'\nversion='0.1.0'\n",
    )
    .unwrap();
    fs::write(
        domain.join("Cargo.toml"),
        "[package]\nname='domain'\nversion='0.1.0'\n",
    )
    .unwrap();

    let mut architecture = load();
    architecture.owners.get_mut("root").unwrap().path = root.to_string_lossy().into_owned();
    architecture.owners.get_mut("domain").unwrap().path = domain.to_string_lossy().into_owned();
    architecture
        .owners
        .get_mut("root")
        .unwrap()
        .allowed_dependencies
        .clear();
    let metadata = CargoMetadata {
        packages: vec![
            CargoPackage {
                id: "root-id".into(),
                manifest_path: root.join("Cargo.toml").to_string_lossy().into_owned(),
                dependencies: vec![CargoDependency {
                    name: "domain".into(),
                    path: Some(domain.to_string_lossy().into_owned()),
                }],
            },
            CargoPackage {
                id: "domain-id".into(),
                manifest_path: domain.join("Cargo.toml").to_string_lossy().into_owned(),
                dependencies: vec![],
            },
        ],
        workspace_members: ["root-id".into(), "domain-id".into()].into(),
    };
    let error = validate_cargo_edges(&architecture, &metadata).unwrap_err();
    assert!(error.contains("root has forbidden real Cargo edge to domain"));
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
            assert!(text.contains("# ADR-007: Rsbuild/Rspack Frontend Build"));
            assert!(text.contains("Accepted. This ADR records the build system already used"));
            assert!(text.contains("dashboard/build.shared.mjs"));
            assert!(text.contains("historical scenario as a migration request"));
            assert!(!text.contains("Pending measured Rsbuild-versus-Vite comparison"));
            assert!(!text.contains("evidence/frontend-build-comparison.md"));
        }
    }
}
