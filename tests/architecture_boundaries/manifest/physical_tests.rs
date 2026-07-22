use super::physical::{inspect_physical_manifest_paths, workspace_manifest_paths};
use crate::query_kernel::query_kernel_violations;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

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
            "[package]\nname = \"engine\"\nversion = \"0.1.0\"\n[lib]\nname = \"query_engine\"\npath = \"src/core.rs\"\n",
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
fn write_required_manifests(repository: &Path) -> Vec<PathBuf> {
    let manifests = workspace_manifest_paths().expect("load workspace manifests");
    for path in &manifests {
        let path = repository.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n").unwrap();
    }
    manifests.into_iter().collect()
}

#[cfg(unix)]
#[test]
fn physical_manifest_contract_rejects_symlinked_crates() {
    let temporary = tempfile::tempdir().expect("create symlinked manifest fixture");
    let repository = temporary.path();
    let mut tracked = write_required_manifests(repository);
    fs::create_dir_all(repository.join("components")).unwrap();
    symlink(
        repository.join("crates/tracedecay-domain"),
        repository.join("components/engine"),
    )
    .unwrap();
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
    let mut tracked = write_required_manifests(repository);
    fs::create_dir_all(repository.join("src/query")).unwrap();
    fs::create_dir_all(repository.join("shared")).unwrap();
    fs::write(repository.join("src/query/mod.rs"), "mod safe;\n").unwrap();
    fs::write(repository.join("shared/safe.rs"), "pub struct Safe;\n").unwrap();
    symlink(
        repository.join("shared/safe.rs"),
        repository.join("src/query/safe.rs"),
    )
    .unwrap();
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
