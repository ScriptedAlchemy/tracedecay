//! A binary built from a checkout must say which commit it came from.
//!
//! Successive checkout builds can share the same released version, so the
//! semver alone cannot tell two checkout binaries apart. These tests pin the
//! SemVer build metadata that does, and pin that the reported version still
//! starts with the release-plz-owned version so nothing that compares
//! precedence is disturbed.

use std::path::Path;
use std::process::Command;

use tracedecay::version::{PACKAGE_VERSION, build_identity, build_version};

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
}

/// Which branch runs depends on where the binary was built from: a developer
/// checkout has a commit to name, a packaged or registry build does not. The
/// no-git side of the probe itself is covered by `version::build_identity`'s
/// unit tests, which can construct both worlds.
#[test]
fn version_flag_names_the_commit_when_there_is_one_and_bare_semver_otherwise() {
    let version = reported_version();

    let Some(_) = build_identity::resolve(checkout()).sha else {
        assert_eq!(
            version, PACKAGE_VERSION,
            "a build with no commit to name must report the released version unchanged, \
             with no trailing `+` for a SemVer parser to reject"
        );
        return;
    };

    let metadata = version
        .strip_prefix(&format!("{PACKAGE_VERSION}+"))
        .unwrap_or_else(|| panic!("a checkout build must report build metadata, got {version:?}"));
    let sha = metadata.strip_suffix(".dirty").unwrap_or(metadata);
    assert!(
        sha.len() == 12 && sha.chars().all(|c| c.is_ascii_hexdigit()),
        "{sha:?} is not a short commit sha (from {version:?})"
    );

    // Independently resolve the reported sha against this checkout: a
    // hard-coded or fabricated suffix would not name a real commit.
    let resolved = Command::new("git")
        .arg("-C")
        .arg(checkout())
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{sha}^{{commit}}"),
        ])
        .output()
        .expect("git should run");
    assert!(
        resolved.status.success(),
        "`--version` reported {version:?}, but {sha} is not a commit in this checkout"
    );
}

/// The CLI and daemon service probes compare this value with the daemon's
/// advertised `serverInfo`. Both break silently if the CLI ever prints
/// something else.
#[test]
fn version_flag_matches_the_version_the_library_reports() {
    assert_eq!(reported_version(), build_version());
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
/// checks, and release-plz all still see the published version.
#[test]
fn the_reported_version_still_begins_with_the_released_version() {
    let version = reported_version();
    assert!(
        version.starts_with(PACKAGE_VERSION),
        "{version:?} must begin with the released version {PACKAGE_VERSION:?}"
    );
    let metadata = &version[PACKAGE_VERSION.len()..];
    assert!(
        metadata.is_empty() || metadata.starts_with('+'),
        "anything after the released version must be SemVer build metadata, got {metadata:?}"
    );
}
