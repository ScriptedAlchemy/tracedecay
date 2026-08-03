use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

const INTERNAL_CRATES: &[&str] = &[
    "tracedecay-agent-hosts",
    "tracedecay-automation",
    "tracedecay-capture",
    "tracedecay-code-extraction",
    "tracedecay-code-index",
    "tracedecay-dashboard-api",
    "tracedecay-domain",
    "tracedecay-jsonrpc",
    "tracedecay-lsp",
    "tracedecay-migrate",
    "tracedecay-runtime-core",
    "tracedecay-sessions",
    "tracedecay-usecases",
];

const OMITTED_PR421_CRATES: &[&str] = &[
    "tracedecay-api",
    "tracedecay-application",
    "tracedecay-global-db",
    "tracedecay-host-integration",
    "tracedecay-hooks",
    "tracedecay-policy",
    "tracedecay-query",
    "tracedecay-rusqlite-parity",
    "tracedecay-rusqlite-runtime",
    "tracedecay-sdk",
    "tracedecay-search-eval",
    "tracedecay-semantic",
    "tracedecay-sqlite-parity-protocol",
    "tracedecay-store",
    "tracedecay-temporal-query",
    "tracedecay-tool-catalog",
];

const ALLOWED_INTERNAL_EDGES: &[(&str, &str)] = &[
    ("tracedecay", "tracedecay-agent-hosts"),
    ("tracedecay", "tracedecay-automation"),
    ("tracedecay", "tracedecay-capture"),
    ("tracedecay", "tracedecay-code-extraction"),
    ("tracedecay", "tracedecay-code-index"),
    ("tracedecay", "tracedecay-dashboard-api"),
    ("tracedecay", "tracedecay-domain"),
    ("tracedecay", "tracedecay-jsonrpc"),
    ("tracedecay", "tracedecay-lsp"),
    ("tracedecay", "tracedecay-migrate"),
    ("tracedecay", "tracedecay-runtime-core"),
    ("tracedecay", "tracedecay-sessions"),
    ("tracedecay", "tracedecay-usecases"),
    ("tracedecay-agent-hosts", "tracedecay-automation"),
    ("tracedecay-agent-hosts", "tracedecay-lsp"),
    ("tracedecay-agent-hosts", "tracedecay-runtime-core"),
    ("tracedecay-agent-hosts", "tracedecay-sessions"),
    ("tracedecay-code-extraction", "tracedecay-domain"),
    ("tracedecay-code-index", "tracedecay-code-extraction"),
    ("tracedecay-dashboard-api", "tracedecay-agent-hosts"),
    ("tracedecay-dashboard-api", "tracedecay-automation"),
    ("tracedecay-dashboard-api", "tracedecay-code-index"),
    ("tracedecay-dashboard-api", "tracedecay-domain"),
    ("tracedecay-dashboard-api", "tracedecay-lsp"),
    ("tracedecay-dashboard-api", "tracedecay-runtime-core"),
    ("tracedecay-dashboard-api", "tracedecay-sessions"),
    ("tracedecay-dashboard-api", "tracedecay-usecases"),
    ("tracedecay-migrate", "tracedecay-runtime-core"),
    ("tracedecay-migrate", "tracedecay-sessions"),
    ("tracedecay-runtime-core", "tracedecay-automation"),
    ("tracedecay-runtime-core", "tracedecay-capture"),
    ("tracedecay-runtime-core", "tracedecay-domain"),
    ("tracedecay-runtime-core", "tracedecay-lsp"),
    ("tracedecay-sessions", "tracedecay-runtime-core"),
    ("tracedecay-usecases", "tracedecay-automation"),
    ("tracedecay-usecases", "tracedecay-runtime-core"),
];

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    workspace_members: BTreeSet<String>,
}

#[derive(Deserialize)]
struct Package {
    id: String,
    name: String,
    manifest_path: String,
    dependencies: Vec<Dependency>,
}

#[derive(Deserialize)]
struct Dependency {
    name: String,
    kind: Option<String>,
    rename: Option<String>,
}

#[test]
fn workspace_architecture_contract() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new("cargo")
        .current_dir(repository)
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .output()
        .expect("run cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let metadata: Metadata = serde_json::from_slice(&output.stdout).expect("parse cargo metadata");
    let packages: BTreeMap<_, _> = metadata
        .packages
        .iter()
        .map(|package| (package.id.as_str(), package))
        .collect();
    let workspace: Vec<_> = metadata
        .workspace_members
        .iter()
        .map(|id| {
            packages
                .get(id.as_str())
                .expect("workspace package is present")
        })
        .collect();
    let names: BTreeSet<_> = workspace
        .iter()
        .map(|package| package.name.as_str())
        .collect();
    let expected: BTreeSet<_> = std::iter::once("tracedecay")
        .chain(INTERNAL_CRATES.iter().copied())
        .collect();
    assert_eq!(
        names, expected,
        "workspace must contain the root plus 13 crates"
    );

    for package in &workspace {
        if package.name != "tracedecay" {
            assert!(
                package
                    .manifest_path
                    .ends_with(&format!("crates/{}/Cargo.toml", package.name)),
                "{} is not an internal crate",
                package.name
            );
        }
    }
    for omitted in OMITTED_PR421_CRATES {
        assert!(
            !names.contains(omitted),
            "omitted PR #421 crate is present: {omitted}"
        );
    }

    let allowed: BTreeSet<_> = ALLOWED_INTERNAL_EDGES.iter().copied().collect();
    for package in workspace {
        for dependency in &package.dependencies {
            if dependency.kind.as_deref() == Some("dev")
                || !names.contains(dependency.name.as_str())
            {
                continue;
            }
            assert!(
                allowed.contains(&(package.name.as_str(), dependency.name.as_str())),
                "forbidden internal edge: {} -> {}",
                package.name,
                dependency.name
            );
            assert!(
                package.name == "tracedecay" || dependency.name != "tracedecay",
                "internal crate has a root backedge: {}",
                package.name
            );
        }
        for dependency in &package.dependencies {
            let dependency_alias = dependency.rename.as_deref().unwrap_or_default();
            assert!(
                !dependency.name.to_ascii_lowercase().contains("rusqlite")
                    && !dependency_alias.to_ascii_lowercase().contains("rusqlite"),
                "{} has a forbidden rusqlite dependency",
                package.name
            );
        }
    }
}
