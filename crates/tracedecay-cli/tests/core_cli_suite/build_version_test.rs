//! A tracedecay binary must always say which commit it came from.
//!
//! Successive checkout builds can share the same released version, so the
//! semver alone cannot tell two binaries apart. Build provenance is now
//! mandatory: `build.rs` fails without a verified git worktree, a release sha
//! env, or `cargo package` VCS metadata, so the bare-version case no longer
//! exists. These tests pin the `"{version}+{40-hex-sha}[.dirty]"` shape and
//! pin that the reported version still starts with the Release Please-owned
//! version so nothing that compares precedence is disturbed.

use std::process::Command;

use tracedecay_project::version::PACKAGE_VERSION;

/// The version `tracedecay --version` reports, with the `tracedecay ` prefix
/// clap prints stripped off.
fn reported_version() -> String {
    let output = Command::new(crate::tracedecay_exe())
        .arg("--version")
        .output()
        .expect("the built tracedecay binary should run");
    assert!(
        output.status.success(),
        "`tracedecay --version` exited with {}",
        output.status
    );
    let printed = String::from_utf8_lossy(&output.stdout).trim().to_string();
    printed
        .strip_prefix("tracedecay ")
        .unwrap_or_else(|| panic!("unexpected `--version` output: {printed:?}"))
        .to_string()
}

/// The full sha embedded in the reported version, after pinning the
/// `"{PACKAGE_VERSION}+{40-hex}[.dirty]"` shape.
fn assert_reported_build_metadata(version: &str) {
    let metadata = version
        .strip_prefix(&format!("{PACKAGE_VERSION}+"))
        .unwrap_or_else(|| {
            panic!(
                "every build must report build metadata after the released version, got {version:?}"
            )
        });
    let sha = metadata.strip_suffix(".dirty").unwrap_or(metadata);
    assert!(
        sha.len() == 40 && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
        "{sha:?} is not a full 40-hex lowercase commit sha (from {version:?})"
    );
}

/// Compare against the same stamped source identity consumed by the CLI build;
/// the runfiles tree deliberately contains no Git checkout metadata.
#[test]
fn version_flag_always_names_the_exact_commit() {
    let version = reported_version();
    assert_reported_build_metadata(&version);
    let source_identity = std::fs::read_to_string("../../product_git_sha.txt")
        .expect("the canonical stamped source identity must be declared as test data");
    assert_eq!(
        version,
        format!("{PACKAGE_VERSION}+{}", source_identity.trim()),
        "the reported version must match the stamped source identity, including dirtiness",
    );
}

/// `tracedecay-agent-hosts` stamps this version into every plugin manifest,
/// plugin cache path, and staleness warning a host can see, but `env!` resolves
/// per compiled crate, so its own `CARGO_PKG_VERSION` is the sub-crate's. Its
/// `PRODUCT_VERSION` is baked from the root package instead; if that wiring
/// ever breaks, hosts silently compare deployed plugins against a version no
/// release ever had.
#[test]
fn the_agent_host_bundles_are_stamped_with_this_packages_version() {
    assert_eq!(tracedecay_agent_hosts::PRODUCT_VERSION, PACKAGE_VERSION);
}
