use super::cargo::parse_cargo_source_layout;
use super::fixture::{
    FIXTURE_SCHEMA_VERSION, fixture_document, load_workspace_snapshot, snapshot_hash,
};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn frozen_metadata(repository: &Path) -> serde_json::Value {
    let snapshot = load_workspace_snapshot().expect("load workspace snapshot");
    for target in &snapshot.targets {
        let path = repository.join(&target.3);
        fs::create_dir_all(path.parent().expect("target parent")).expect("create target parent");
        fs::write(path, "").expect("create target source");
    }
    let packages = snapshot
        .packages
        .iter()
        .map(|package| {
            let mut dependencies: Vec<_> = package
                .exact_dependencies
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|dependency| {
                    serde_json::json!({
                        "name": dependency.0,
                        "kind": dependency.1.as_str(),
                    })
                })
                .collect();
            if package.manifest == "Cargo.toml" {
                dependencies.extend(
                    snapshot
                        .root_package_aliases
                        .iter()
                        .map(|alias| serde_json::json!({ "name": alias.1, "rename": alias.0 })),
                );
            }
            let targets: Vec<_> = snapshot
                .targets
                .iter()
                .filter(|target| target.0 == package.package)
                .map(|target| {
                    serde_json::json!({
                        "name": target.1,
                        "kind": [target.2.as_str()],
                        "src_path": repository.join(&target.3),
                    })
                })
                .collect();
            serde_json::json!({
                "id": package.package,
                "name": package.package,
                "manifest_path": repository.join(&package.manifest),
                "dependencies": dependencies,
                "targets": targets,
            })
        })
        .collect::<Vec<_>>();
    let workspace_members = snapshot
        .packages
        .iter()
        .map(|package| package.package.clone())
        .collect::<Vec<_>>();
    serde_json::json!({
        "packages": packages,
        "workspace_members": workspace_members,
    })
}

#[test]
fn workspace_snapshot_fixture_has_exact_version_hash_and_structure() {
    let (version, expected_hash, snapshot) = fixture_document().expect("parse fixture document");
    assert_eq!(version, FIXTURE_SCHEMA_VERSION);
    assert_eq!(
        expected_hash,
        snapshot_hash(&snapshot).expect("hash fixture snapshot")
    );
    assert_eq!(
        load_workspace_snapshot()
            .expect("validate fixture")
            .targets
            .len(),
        snapshot.targets.len()
    );
}

#[test]
fn metadata_layout_includes_workspace_targets_and_scopes_tracked_sources() {
    let temporary = tempfile::tempdir().expect("create metadata fixture");
    let repository = temporary.path();
    let metadata = frozen_metadata(repository);
    let layout = parse_cargo_source_layout(
        repository,
        &serde_json::to_vec(&metadata).expect("serialize metadata fixture"),
    )
    .expect("parse metadata fixture");
    let snapshot = load_workspace_snapshot().expect("load workspace snapshot");
    let expected_targets = snapshot
        .targets
        .iter()
        .map(|target| PathBuf::from(&target.3))
        .collect();
    assert_eq!(layout.target_roots, expected_targets);

    let mut expected_tracked: BTreeSet<PathBuf> = ["benches", "examples", "src", "tests"]
        .into_iter()
        .map(PathBuf::from)
        .collect();
    expected_tracked.extend(snapshot.packages.iter().filter_map(|package| {
        let parent = Path::new(&package.manifest).parent()?;
        (!parent.as_os_str().is_empty()).then(|| parent.to_path_buf())
    }));
    for target in &layout.target_roots {
        if !expected_tracked.iter().any(|root| target.starts_with(root)) {
            expected_tracked.insert(target.clone());
        }
    }
    assert_eq!(layout.tracked_roots, expected_tracked);
    assert_eq!(
        layout.workspace_manifests,
        snapshot
            .packages
            .iter()
            .map(|package| PathBuf::from(&package.manifest))
            .collect()
    );
    assert!(
        layout.pr8_violations.is_empty(),
        "exact frozen workspace snapshot must be admitted: {:?}",
        layout.pr8_violations
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
                    { "name": "sqlx", "rename": "serde" },
                    { "name": "tracedecay-rusqlite-runtime" }
                ],
                "targets": [
                    { "kind": ["lib"], "name": "tracedecay", "src_path": repository.join("src/lib.rs") },
                    { "kind": ["bin"], "name": "temporal-kernel", "src_path": repository.join("src/engine.rs") },
                    { "kind": ["example"], "name": "neutral_example", "src_path": repository.join("examples/neutral.rs") },
                    { "kind": ["test"], "name": "neutral_test", "src_path": repository.join("tests/neutral.rs") },
                    { "kind": ["custom-build"], "name": "build-script-build", "src_path": repository.join("build-neutral.rs") }
                ]
            },
            { "id": domain_id, "name": "tracedecay-domain", "manifest_path": repository.join("crates/tracedecay-domain/Cargo.toml"), "targets": [] },
            {
                "id": store_id,
                "name": "tracedecay-store",
                "manifest_path": repository.join("crates/tracedecay-store/Cargo.toml"),
                "dependencies": [
                    { "name": "mongodb", "rename": "serde_json" },
                    { "name": "rusqlite" },
                    { "name": "tracedecay-rusqlite-runtime" }
                ],
                "targets": []
            },
            { "id": neutral_id, "name": "engine", "manifest_path": repository.join("components/engine/Cargo.toml"), "targets": [] }
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
        "contract dependency rusqlite -> rusqlite",
        "contract dependency tracedecay-rusqlite-runtime -> tracedecay-rusqlite-runtime",
        "temporal-kernel",
        "neutral_example",
        "neutral_test",
        "build-neutral.rs",
        "components/engine",
    ] {
        assert!(
            layout
                .pr8_violations
                .iter()
                .any(|violation| violation.contains(expected)),
            "metadata contract missed {expected}: {:?}",
            layout.pr8_violations
        );
    }
}

#[test]
fn storage_runtime_manifest_roles_require_exact_dependencies_and_paths() {
    let temporary = tempfile::tempdir().expect("create storage runtime metadata fixture");
    let repository = temporary.path();
    let protocol_id = "protocol";
    let parity_id = "parity";
    let runtime_id = "runtime";
    let parity_lookalike_id = "parity-lookalike";
    let runtime_lookalike_id = "runtime-lookalike";
    let metadata = serde_json::json!({
        "packages": [
            {
                "id": protocol_id,
                "name": "tracedecay-sqlite-parity-protocol",
                "manifest_path": repository.join("crates/tracedecay-sqlite-parity-protocol/Cargo.toml"),
                "dependencies": [{ "name": "rusqlite" }, { "name": "tempfile" }],
                "targets": [
                    { "kind": ["lib"], "name": "tracedecay_sqlite_parity_protocol", "src_path": repository.join("crates/tracedecay-sqlite-parity-protocol/src/lib.rs") },
                    { "kind": ["lib"], "name": "tracedecay_sqlite_parity_protocol_shadow", "src_path": repository.join("crates/tracedecay-sqlite-parity-protocol/src/shadow.rs") }
                ]
            },
            {
                "id": parity_id,
                "name": "tracedecay-rusqlite-parity",
                "manifest_path": repository.join("crates/tracedecay-rusqlite-parity/Cargo.toml"),
                "dependencies": [{ "name": "tracedecay-sqlite-parity-protocol", "rename": "wire" }],
                "targets": []
            },
            {
                "id": runtime_id,
                "name": "tracedecay-rusqlite-runtime",
                "manifest_path": repository.join("crates/tracedecay-rusqlite-runtime/Cargo.toml"),
                "dependencies": [
                    { "name": "rusqlite" },
                    { "name": "tokio" },
                    { "name": "tracedecay-store", "rename": "store" },
                    { "name": "libsql" },
                    { "name": "tempfile", "kind": "dev" }
                ],
                "targets": [
                    { "kind": ["lib"], "name": "tracedecay_rusqlite_runtime", "src_path": repository.join("crates/tracedecay-rusqlite-runtime/src/lib.rs") },
                    { "kind": ["bin"], "name": "tracedecay-rusqlite-runtime-shadow", "src_path": repository.join("crates/tracedecay-rusqlite-runtime/src/shadow.rs") }
                ]
            },
            {
                "id": parity_lookalike_id,
                "name": "tracedecay-rusqlite-parity-shadow",
                "manifest_path": repository.join("crates/tracedecay-rusqlite-parity-shadow/Cargo.toml"),
                "targets": [{ "kind": ["bin"], "name": "tracedecay-rusqlite-parity-shadow", "src_path": repository.join("crates/tracedecay-rusqlite-parity-shadow/src/main.rs") }]
            },
            {
                "id": runtime_lookalike_id,
                "name": "tracedecay-rusqlite-runtime-shadow",
                "manifest_path": repository.join("crates/tracedecay-rusqlite-runtime-shadow/Cargo.toml"),
                "targets": [{ "kind": ["lib"], "name": "tracedecay_rusqlite_runtime_shadow", "src_path": repository.join("crates/tracedecay-rusqlite-runtime-shadow/src/lib.rs") }]
            }
        ],
        "workspace_members": [protocol_id, parity_id, runtime_id, parity_lookalike_id, runtime_lookalike_id]
    });
    let layout = parse_cargo_source_layout(
        repository,
        &serde_json::to_vec(&metadata).expect("serialize metadata fixture"),
    )
    .expect("parse metadata fixture");

    for expected in [
        "driver-free SQLite parity protocol dependency is forbidden: rusqlite|normal",
        "driver-free SQLite parity protocol dependency is missing: hex|normal",
        "driver-free SQLite parity protocol dependency is forbidden: tempfile|normal",
        "driver-free SQLite parity protocol dependency is missing: tempfile|dev",
        "process-isolated rusqlite parity probe must not rename dependency wire -> tracedecay-sqlite-parity-protocol",
        "private bundled rusqlite storage runtime dependency is missing: serde_json|normal",
        "private bundled rusqlite storage runtime dependency is forbidden: libsql|normal",
        "private bundled rusqlite storage runtime must not rename dependency store -> tracedecay-store",
        "additional workspace contract member is forbidden: crates/tracedecay-rusqlite-parity-shadow/Cargo.toml",
        "additional workspace contract member is forbidden: crates/tracedecay-rusqlite-runtime-shadow/Cargo.toml",
        "additional workspace Cargo target is forbidden: tracedecay-sqlite-parity-protocol|tracedecay_sqlite_parity_protocol_shadow|lib|crates/tracedecay-sqlite-parity-protocol/src/shadow.rs",
        "additional workspace Cargo target is forbidden: tracedecay-rusqlite-runtime|tracedecay-rusqlite-runtime-shadow|bin|crates/tracedecay-rusqlite-runtime/src/shadow.rs",
        "additional workspace Cargo target is forbidden: tracedecay-rusqlite-parity-shadow|tracedecay-rusqlite-parity-shadow|bin|crates/tracedecay-rusqlite-parity-shadow/src/main.rs",
        "additional workspace Cargo target is forbidden: tracedecay-rusqlite-runtime-shadow|tracedecay_rusqlite_runtime_shadow|lib|crates/tracedecay-rusqlite-runtime-shadow/src/lib.rs",
    ] {
        assert!(
            layout
                .pr8_violations
                .iter()
                .any(|violation| violation.contains(expected)),
            "storage runtime manifest contract missed {expected}: {:?}",
            layout.pr8_violations
        );
    }
}
