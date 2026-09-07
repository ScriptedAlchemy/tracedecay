//! A tracedecay binary must always say which commit it came from.
//!
//! Successive checkout builds can share the same released version, so the
//! semver alone cannot tell two binaries apart. Build provenance is now
//! mandatory: `build.rs` fails without a verified git worktree, a release sha
//! env, or `cargo package` VCS metadata, so the bare-version case no longer
//! exists. These tests pin the `"{version}+{40-hex-sha}[.dirty]"` shape and
//! pin that the reported version still starts with the Release Please-owned
//! version so nothing that compares precedence is disturbed.

use std::path::Path;
use std::process::Command;

use tracedecay::version::PACKAGE_VERSION;

/// The version `tracedecay --version` reports, with the `tracedecay ` prefix
/// clap prints stripped off.
fn reported_version() -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_tracedecay"))
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

fn checkout() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the CLI crate should live under the workspace crates directory")
}

/// The full sha embedded in the reported version, after pinning the
/// `"{PACKAGE_VERSION}+{40-hex}[.dirty]"` shape.
fn reported_full_sha(version: &str) -> String {
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
    sha.to_string()
}

/// A hard-coded or fabricated sha would not match the checkout this test
/// binary was built from: when the suite runs from a git checkout, the
/// reported commit must be exactly the checkout's `HEAD`.
#[test]
fn version_flag_always_names_the_exact_commit() {
    let version = reported_version();
    let sha = reported_full_sha(&version);

    let head = Command::new("git")
        .arg("-C")
        .arg(checkout())
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git should run");
    if !head.status.success() {
        // Built from a non-checkout source (release env or packaged VCS
        // metadata); the shape assertions above are the contract there.
        return;
    }
    let head = String::from_utf8_lossy(&head.stdout).trim().to_string();
    assert_eq!(
        sha, head,
        "`--version` reported {version:?}, but this checkout's HEAD is {head}"
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

/// Build metadata is ignored for precedence, so release comparisons, upgrade
/// checks and Release Please still see the released version.
#[test]
fn the_reported_version_still_begins_with_the_released_version() {
    let version = reported_version();
    assert!(
        version.starts_with(PACKAGE_VERSION),
        "{version:?} must begin with the released version {PACKAGE_VERSION:?}"
    );
    let metadata = &version[PACKAGE_VERSION.len()..];
    assert!(
        metadata.starts_with('+'),
        "everything after the released version must be SemVer build metadata, got {metadata:?}"
    );
}
