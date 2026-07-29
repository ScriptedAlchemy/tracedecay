//! Final workspace dependency-direction guard.

use std::collections::BTreeSet;
use std::env;
use std::path::PathBuf;
use std::process::Command;

use serde::Deserialize;

#[derive(Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
    workspace_members: BTreeSet<String>,
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
fn extracted_workspace_crates_do_not_depend_on_the_root_package() {
    let metadata = cargo_metadata();
    let workspace_packages = metadata
        .packages
        .iter()
        .filter(|package| metadata.workspace_members.contains(&package.id))
        .collect::<Vec<_>>();
    assert!(
        workspace_packages
            .iter()
            .any(|package| package.name == "tracedecay"),
        "root package must remain in the workspace"
    );

    let violations = workspace_packages
        .into_iter()
        .filter(|package| package.name != "tracedecay")
        .filter(|package| {
            package
                .dependencies
                .iter()
                .any(|dependency| dependency.name == "tracedecay")
        })
        .map(|package| package.name.as_str())
        .collect::<Vec<_>>();

    assert!(
        violations.is_empty(),
        "extracted crates must remain independently reusable and cannot depend on the root package: {}",
        violations.join(", ")
    );
}
